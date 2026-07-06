# Phase C2 — optional task+mesh-shader tessellation front end

**Status:** IMPLEMENTED (device support + meshlet clustering + GPU upload + the task/mesh shaders + the
graphics-pipeline object + the `cmd_draw_mesh_tasks` scene-pass draw — all compiling + `clippy -D
warnings`-clean + capability-and-opt-in-gated). The whole front end is built end-to-end; it is gated
behind `mesh_shader_supported()` **and** the `SAFFRON_MESH_SHADER` env opt-in, so the validated
index-draw path is the untouched default. The one thing this environment cannot do is *render through*
it — llvmpipe advertises no `VK_EXT_mesh_shader`, so the path never engages here — meaning the final
visual/validation-layer confirmation is a pass on the NVIDIA card (`SAFFRON_MESH_SHADER=1 just run`).
Because the path is opt-in and default-off, that outstanding check is zero-risk to the shipped renderer.
**Scope:** `saffron-geometry` (meshlet clustering), `saffron-rendering` (device + upload + pipeline/draw)
**Depends on:** phase-b2/c1 + the per-vertex tangent stream (built in B3)

## What is built (compiling + gated; renders on mesh-shader hardware)

- **Meshlet clustering** — `saffron_geometry::build_meshlets` (`crates/geometry/src/meshlet.rs`): a greedy
  per-submesh clusterer (≤ 64 verts, ≤ 124 tris per meshlet) producing the standard three-array form
  (`Meshlet` descriptors with a bounding sphere + flat global vertex indices + packed `u8` local
  triangle indices) plus per-submesh meshlet ranges. Five unit tests (split-on-overflow, local-index
  round-trip, submesh partitioning).
- **Device support** — `VK_EXT_mesh_shader` detection (`Capabilities::mesh_shader_supported`), extension
  + `PhysicalDeviceMeshShaderFeaturesEXT` (mesh + task) enablement, and the `cmd_draw_mesh_tasks`
  dispatch, all mirroring the existing `rt_supported` gate (`crates/rendering/src/device.rs`).
- **GPU upload** — `GpuMesh` carries `MeshletBuffers` (descriptor + meshlet-vertex + meshlet-triangle
  storage buffers), built + uploaded per mesh when the device supports mesh shaders
  (`Uploader::upload_meshlet_buffers`, `crates/rendering/src/upload.rs`).
- **Shaders** — `assets/shaders/meshlet.slang`: a task (amplification) shader that frustum-culls each
  meshlet by its bounding sphere and compacts survivors, and a mesh shader that fetches vertices from
  the base stream, transforms them (mirroring `lighting::transformVertex`, reading its own `viewProj`
  push + the shared `instances` buffer), and emits the *same* `VertexOutput` the übershader
  `mesh::fragmentMain` consumes — so the front end swaps only the geometry stage. Compiles warning-clean
  (`spvMeshShadingEXT` added to the shader capability atom).
- **Graphics pipeline** — `Pipelines::request_meshlet` (`crates/rendering/src/pipelines.rs`) builds the
  `VK_EXT_mesh_shader` PSO lazily and caches it: task+mesh stages from `meshlet.spv`, the übershader
  `fragmentMain` from `mesh.spv`, no vertex-input state (a mesh shader fetches its own geometry), the
  scene depth/MSAA/blend/rendering-format state, and a layout of the übershader sets 0–7 + set 8 (the
  meshlet geometry) + the 80-byte `MeshletPush` range (`MESH_EXT | TASK_EXT`). On a non-RT device (the
  übershader list stops at set 5) sets 6/7 are padded with an unused layout so the geometry set still
  lands at the shader's fixed index 8; the fragment never references the padded sets. Rebuilt on an MSAA
  change (`set_sample_count`).
- **Draw wiring** — `MeshletRaster` (`crates/rendering/src/meshlet_raster.rs`): the set-8 layout (four
  mesh/task storage buffers) + one descriptor pool per frame-in-flight, mirroring `Displacement`'s
  per-frame pool discipline. `wire` resets the frame pool and, per opaque batch → submesh → instance,
  allocates a set 8 (the mesh's meshlet buffers + its vertex stream — the shared deformed buffer for a
  skinned/displaced batch, with `vertex_base` shifting each global index into that batch's deformed
  slice) and records the `MeshletPush`; it is all-or-nothing (`None` → the whole opaque list falls back
  to the index path, never dropping a batch). `record_meshlet_draws` binds the PSO, rebinds sets 0–7
  under the mesh-pipeline layout (its push range differs, so the index path's binds do not carry), then
  per draw binds set 8, pushes, and dispatches `ceil(meshlet_count / 32)` task groups. Engaged from the
  scene `RgPass` only when `mesh_shader_supported()` **and** `SAFFRON_MESH_SHADER` is set
  (`crates/rendering/src/renderer.rs`); the meshlet subsystem is `None` and never touched otherwise.

## What remains (needs the NVIDIA card — a visual + validation-layer pass)

- Nothing to build. The only outstanding step is *running* it on mesh-shader hardware
  (`SAFFRON_MESH_SHADER=1 just run` on the NVIDIA card) to confirm — with eyes + the validation layer —
  that the meshlet output matches the index path pixel-for-pixel and that the sets-0–7-then-8 layout
  binds clean. This is a check, not a code gap, and it is zero-risk to the default renderer because the
  path is opt-in and default-off.

## Goal

Migrate the compute tessellator's *front end* to a **task + mesh-shader (`VK_EXT_mesh_shader`)
amplification path** for finer per-meshlet LOD and in-pipeline geometry generation — the model that
officially replaces the fixed-function tessellator. Keep the compute-buffer → BLAS path for RT.

## Approach

The task (amplification) shader is a programmable, unconstrained tessellator: it dispatches a variable
number of mesh workgroups per LOD, and the mesh shader emits displaced meshlets. This composes with the
GPU-driven culling / mesh-shader work AGENTS.md lists as "not yet" — do it **only** once that roadmap is
underway, so the whole front end is built once on mesh shaders rather than retrofitted.

## Touch points

- A task+mesh pipeline stage in `saffron-rendering`; meshlet-based adaptive displacement; per-primitive
  data via `PerPrimitiveEXT`.

## Verification

- Displaced meshlets match the compute-path output at equal LOD; per-meshlet LOD reduces triangle count
  on distant geometry.

## Notes

- Deferred and optional. The compute-to-buffer path (B2) + BLAS refit (C1) is the correct, portable
  system; this phase is a performance/architecture upgrade gated on the mesh-shader roadmap, not a
  requirement.
