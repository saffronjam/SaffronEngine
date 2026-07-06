# Phase 2 — Import-time watertight conditioning + min-max pyramid (self-verifiable)

**Status:** NOT STARTED

Produce, entirely offline, the data the watertightness chain needs — edge adjacency, a welded
base mesh with per-welded-vertex displacement directions and a seam-consistent tangent seed, UV-seam
height **value** agreement, and a per-height-texture min-max pyramid. Every piece is verified by
**self-contained CPU unit tests** that do not depend on the later GPU dicer (Phase 4). This is the
foundation half of the design: three of the four watertightness guarantees (identical direction,
welded seam-consistent tangent, welded direction-consistent base) plus the seam-value guarantee are
*constructed here at import*; Phase 3's per-edge factor and Phase 4's dice/weld consume them.

## Goal

At mesh import in `saffron-geometry`, condition every imported `Mesh` into a watertight base:

- **(a) Edge adjacency** — a unique-edge list where each triangle references its three shared edges by
  index, so a per-edge tessellation factor (Phase 3) is indexable identically by both incident
  triangles.
- **(b) Spatial-hash weld** — collapse coincident positions, average+renormalize the displacement
  **direction**, and re-derive a **seam-consistent tangent seed** from the 48 B `Vertex` tangent frame
  (the shading-basis half of the direction/tangent guarantee).
- **(c) UV-seam height VALUE agreement** — detect seam edges (one 3D edge, different UVs on the two
  incident triangles) and guarantee both sides sample an **equal** height value, either by author-time
  seam-aware dilation with a **verified** cross-seam texel match or by a per-seam-edge
  object-space/triplanar sampling flag.
- **(d) Serialize + upload** — bump `MESH_FORMAT_VERSION`, bake the per-edge / per-welded-vertex / seam
  data into the `.smesh`, and carry it through `GpuMesh` upload as additional storage buffers.

Separately, at height-texture upload, build a per-texture **min-max (max-mipmap) pyramid** stored beside
the bindless height texture, for the Phase-9 prism march and the D3 offset-limiting parallax reuse.

Nothing in Phase 2 changes what the renderer draws — the new buffers are produced and uploaded but
unconsumed until Phase 3. The milestone gate must stay green with the data present-but-idle.

## Build plan (grounded in the real import + upload code)

### (a) Edge adjacency — a new `saffron-geometry` conditioning module

The `Mesh` type is `{ vertices: Vec<Vertex>, indices: Vec<u32>, submeshes: Vec<Submesh> }`
(`crates/geometry/src/types.rs`). Add a `conditioning.rs` module beside `types.rs` producing a
`MeshConditioning` aggregate. Build order is **weld first, adjacency second** (§b then §a) because edges
must be keyed on **welded** identity, not on attribute-split base-vertex identity — otherwise a UV seam
(two base vertices at one 3D position with different UVs) reads as a mesh boundary and its factor is
never shared.

Edge structures (all `#[repr(C)] Pod`, so they serialize and upload as storage buffers with no
re-pack):

- `Edge { v0: u32, v1: u32, flags: u32 }` — the two **welded** endpoint ids, canonicalized `v0 < v1`,
  plus a bitfield (`BOUNDARY`, `SEAM`, and the seam sampling mode). Unique per canonical
  `(min,max)` welded pair.
- `TriEdges { e: [u32; 3] }` — parallel to `indices.chunks_exact(3)`; each triangle's three edge
  indices into the unique-edge list, so both triangles on a shared edge resolve to the same `Edge`.
- The build counts incident triangles per unique edge: exactly 2 ⇒ interior; exactly 1 ⇒ boundary
  (`Edge::BOUNDARY`); >2 ⇒ non-manifold (record and flag — a non-manifold edge falls back to a
  clamped world-space factor at Phase 3 rather than cracking).

This mirrors nothing existing — the greedy `build_meshlets` clusterer (`crates/geometry/src/meshlet.rs`)
carries no edge/adjacency data — so it is new, but it is a pure function of `Mesh` and fully CPU-testable.

### (b) Spatial-hash weld + per-welded-vertex direction/tangent

`compute_tangents(&mut Mesh)` (`types.rs`, Lengyel + Gram-Schmidt) already runs on every import (glTF
reads the `TANGENT` accessor when present, OBJ + primitives always compute), so every base `Vertex`
arrives with a UV-aligned `tangent: [f32;4]` at offset 32 before conditioning runs. Add
`build_weld(&Mesh) -> WeldMap`:

- Hash quantized positions on a scale-relative epsilon (bound to the mesh AABB from
  `bounds_min`/`bounds_max` so the tolerance is resolution-independent). Produce `weld_id: Vec<u32>`
  (base vertex → welded-vertex id) and the welded-vertex count.
- Per welded vertex, average the contributing base `normal`s and renormalize → the
  **displacement direction** (so all base copies of a shared vertex displace to one 3D point — no
  crack). Average the contributing `tangent.xyz`, Gram-Schmidt against the welded direction, and carry a
  consistent handedness (majority `tangent.w`) → the **seam-consistent tangent seed**. Store
  `WeldedVertex { direction: Vec3, tangent: [f32;4] }` (`#[repr(C)] Pod`).
- The direction is the geometric normal; the tangent seed is the **shading** basis. Phase 4 re-derives
  the final per-micro-vertex tangent from the full position Jacobian, but seeds it from this welded,
  seam-consistent basis so the two incident triangles at a seam start from one agreed frame.

Edge adjacency (§a) then keys on `weld_id`, so a UV-seam split collapses to one welded endpoint pair and
the seam edge is a single shared `Edge` referenced by both triangles.

### (c) UV-seam height VALUE agreement

For each interior edge, compare the UV0 the two incident triangles assign to the shared welded
endpoints. If they differ beyond an epsilon the edge is a **UV seam** (`Edge::SEAM`); watertightness then
requires both triangles to read an **equal** height value there. Resolve per seam edge with one of two
modes, stored in `Edge::flags`:

- **`SEAM_DILATED`** — author-time seam-aware dilation of the height texture: extend each UV island's
  border outward so the texels the two islands read across the shared edge match. Then **verify** the
  cross-seam texel match (sample both islands' edge texels and assert bit-equality within the texture's
  value quantum); a seam that fails to reconcile downgrades to the object-space mode rather than shipping
  a mismatch. Dilation runs in `saffron-assets` at material import (it edits the height image), keyed by
  `MaterialMapRole::Height` (`crates/geometry/src/types.rs`).
- **`SEAM_OBJECT_SPACE`** — flag the seam edge to sample height in object-space/triplanar so both
  triangles read one value independent of UV. No texture edit; Phase 4's dice kernel branches on the
  flag when it samples the shared edge.

The chosen mode is per seam edge, baked into the `.smesh` alongside the seam flag, so Phase 4 reads one
sampling decision and never has to re-derive it.

### (d) Serialize into `.smesh` v5 + upload as storage buffers

The `.smesh` header (`crates/geometry/src/smesh.rs`) is a 64 B `SMeshHeader` with three required sections
(vertices/indices/submeshes) and optional sections behind a flags word (`MESH_FLAG_SKIN = 1<<0`,
`MESH_FLAG_MORPH = 1<<1`), offsets self-relative so an embedded `.smodel` chunk reads identically.

- Bump `MESH_FORMAT_VERSION` 4 → 5 (the const at `smesh.rs:29`; `decode` rejects any other version). This
  invalidates every cached `.smesh` — every asset re-imports at v5, which is fine (data migration is out
  of scope per AGENTS.md; start fresh).
- Add `MESH_FLAG_CONDITIONING = 1<<2` and a conditioning section (after skin/morph): a small sub-header
  (`edge_count`, `welded_count`) then the `Edge[]`, `TriEdges[]`, `WeldedVertex[]`, and `weld_id: u32[]`
  arrays. Extend `encode_mesh_image` / `decode` / add `load_mesh_conditioning_from_bytes` exactly as the
  skin/morph sections do; add the section stride to the 64 B-header + section-offset assertions in the
  `smesh` tests.
- Upload: mirror `MeshletBuffers` (`crates/rendering/src/resources.rs:845`) and its
  `Uploader::upload_meshlet_buffers` (`crates/rendering/src/upload.rs:1484`). Add a `ConditioningBuffers`
  ( `edges`, `tri_edges`, `welded`, `weld_id` — each `STORAGE_BUFFER`, one staging buffer with contiguous
  slices + one `cmd_copy_buffer` per slice, non-fatal on failure like the meshlet path), a
  `GpuMesh.conditioning: Option<ConditioningBuffers>` field threaded through `GpuMeshParts` /
  `GpuMesh::from_parts` / `Drop`, and a `GpuMesh::conditioning()` accessor. No RT/vertex usage yet — these
  are compute-read only in Phase 3, so plain `STORAGE_BUFFER` suffices.

### Min-max (max-mipmap) pyramid at height-texture upload

Height maps upload through `Uploader::upload_texture` (`upload.rs:1597`) as bindless slots in set-0
binding 0 (`descriptors.rs:906`, `COMBINED_IMAGE_SAMPLER`, `FRAGMENT | COMPUTE`); the normal mip chain is
a linear `cmd_blit_image` down-average (`record_mip_chain`, `upload.rs:2662`). A min-max pyramid is
**not** a linear average — each level must be the exact **min and max** of its 2×2 children — so
`record_mip_chain` cannot produce it. Build the pyramid **CPU-side** from the decoded height texels (this
also makes it self-verifiable):

- A pure `build_min_max_pyramid(&[f32], width, height) -> Vec<MinMaxLevel>` (in `saffron-geometry`,
  operating on the height channel), where level 0 is `(h,h)` per texel and each coarser level is the
  per-channel `(min,max)` of its four children, down to 1×1.
- Upload as a companion mipped `R16G16_UNORM` image (R=min, G=max) with its own bindless slot beside the
  height texture. Gate the build on `MaterialMapRole::Height` — thread the role into the material-texture
  upload site (which already distinguishes roles via `detect_material_role` /
  `MaterialMapRole`, `scan.rs`/`types.rs`) so only height maps pay for it.
- The pyramid is **point-sampled with explicit LOD** by its consumers (Phase 9 prism march, D3 parallax),
  never linearly filtered — linear filtering blends min with max and breaks the conservative bound. Note
  this on the image so the consuming phase uses `Load`/nearest, not the shared linear bindless sampler.
- Nothing samples it in Phase 2; the slot is claimed and stashed on the height texture's material entry
  for Phases 9 / D3.

## Scope

- `saffron-geometry` — new `conditioning.rs` (edge adjacency, weld, seam detection, `MeshConditioning`);
  `smesh.rs` v5 encode/decode + `MESH_FLAG_CONDITIONING`; `build_min_max_pyramid`; the importers
  (`gltf_import.rs`, `obj_import.rs`, `primitives.rs`) call conditioning after `compute_tangents`.
- `saffron-assets` — author-time UV-seam dilation on the height image + the cross-seam verify;
  min-max-pyramid upload gated on `MaterialMapRole::Height`.
- `saffron-rendering` — `ConditioningBuffers` + `GpuMesh` field/accessor; `Uploader` conditioning-buffer
  upload (mirroring `upload_meshlet_buffers`) and the height-texture pyramid companion.

## Depends on

Nothing — an independent import/offline foundation (README DAG root: **2 → 3**, `{2,7} → 9`). Reuses the
existing `Vertex` tangent frame + `compute_tangents` (B3), the `.smesh` section machinery, the
`MeshletBuffers` upload pattern, the bindless set-0 height sampling (`descriptors.rs:906`), and
`MaterialMapRole`. Consumed by Phase 3 (per-edge factor over the adjacency + welded data), Phase 4
(dice/displace/weld reads the direction/tangent/seam-mode), and Phase 9 (prism march over the pyramid).

## Verification

**Build / lint gate:** `just engine` then `cargo clippy --workspace -- -D warnings` + `cargo fmt`
clean on `saffron-geometry`, `saffron-assets`, `saffron-rendering`.

**Self-contained CPU unit tests (fix #20 — no GPU, no dependence on the later dicer):**

- *Adjacency correctness* — on a constructed manifold patch, every interior edge is referenced by
  **exactly two** triangles and every boundary edge by **exactly one**; each `TriEdges` entry resolves
  back to an `Edge` whose endpoints are that triangle's welded corner pair; a non-manifold edge is
  flagged, not dropped.
- *Weld correctness* — coincident positions collapse to one welded id; per-welded direction and tangent
  are unit-length, finite, and equal to the renormalized average of the contributors; a UV-seam split
  (same position, different UV) collapses to **one** welded vertex **and** the shared edge is detected as
  a seam.
- *Pyramid correctness* — for a known height array, each level's `(min,max)` equals the exact min/max of
  its 2×2 children at every level down to 1×1 (including odd-extent edge cases).
- *Seam-agree (fix #5)* — a constructed two-triangle asset sharing one 3D edge with divergent UVs resolves
  to a **bit-equal** sampled height on both sides: the `SEAM_DILATED` path verifies the cross-seam texels
  match within the value quantum; a case that cannot reconcile falls to `SEAM_OBJECT_SPACE` and both
  triangles then read one object-space value.
- *`.smesh` v5 roundtrip* — encode → decode preserves the conditioning section byte-for-byte; header
  stride/offset asserts updated; a v4 image is rejected.

**Rest-of-engine tolerance (present-but-unused buffers):** `just check` (workspace build + shaders →
present-only smoke → control-schema contract → frontend build) stays green and the smoke log stays
validation-clean; every builtin/preview/imported mesh re-imports at v5 (bump the asset-cache version so
stale `.smesh` re-bake); RT BLAS + all seven geometry passes are byte-for-byte unaffected because nothing
reads the new buffers yet.

**GPU-with-eyes:** minimal for this phase — Phase 2 is offline conditioning, so a `just run` /
`just run-engine-headless` must render **identically** to before (no visual delta, since no consumer
exists). The real crack-testing — a shared edge with divergent factors staying crack-free, welded tangent
continuity across a seam, and RT silhouette == raster silhouette — lands in Phases 4/6/7 that consume this
data; it cannot be exercised on-GPU here and must not be claimed here.

## Risks

- **UV-seam value agreement is genuine research.** Seam-aware dilation producing a *verified* cross-seam
  texel match is the load-bearing, unproven step; it is not proven until the seam-agree CPU test passes,
  and a wrong result leaks light / cracks at Phase-7 RT. The object-space/triplanar fallback is the safety
  valve, but it changes the sampled value's character (object-space vs UV), so it must be the deliberate
  per-seam choice, not a silent default.
- **Weld tolerance is a two-sided trap.** Too coarse an epsilon merges genuinely distinct vertices (folds
  geometry, wrong direction average); too fine misses a seam split (a crack survives). Pin a
  scale-relative epsilon from the mesh AABB and cover both failure directions in the weld test.
- **Adjacency must key on welded ids, not base ids.** Building edges over attribute-split base vertices
  treats a UV seam as a boundary, so Phase 3 never shares the factor there and the surface cracks — the
  weld strictly precedes adjacency, and the test must assert a seam edge is a single shared `Edge`.
- **`MESH_FORMAT_VERSION` 4 → 5 forces a full re-import.** Every cached `.smesh` (builtin/preview meshes,
  project assets) must re-bake; the asset-cache version has to invalidate or `decode` will reject live
  data. Migration is out of scope (fresh project), but the cache bump is mandatory or the smoke fails.
- **The min-max pyramid cannot reuse `record_mip_chain`** (linear average) and must be point-sampled with
  explicit LOD by its consumers — a linear-filtered read blends min with max and silently breaks the
  conservative bound the Phase-9 prism march relies on. Building the pyramid CPU-side keeps it exact and
  testable but doubles a height map's bindless-slot footprint (bounded by `MAX_BINDLESS_TEXTURES`).
- **Present-but-unused data must stay truly inert.** The new `.smesh` section and storage buffers touch
  the upload path and the format; a stride or offset error there breaks *every* mesh, not just displaced
  ones — the roundtrip test and the unchanged-render smoke are what catch it before Phase 3 depends on it.
