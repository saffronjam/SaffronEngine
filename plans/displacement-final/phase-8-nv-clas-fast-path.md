# Phase 8 — NVIDIA CLAS fast path behind the displaced-geometry → BLAS seam

**Status:** NOT STARTED

**Scope:** `saffron-rendering` (`device.rs` capability probe + extension enable, `rt.rs` build seam +
CLAS backend), optionally `saffron-geometry` (`meshlet.rs` clusterer upgrade). No wire/DTO change.
**Depends on:** [`phase-7-rt-blas-portable-floor.md`](phase-7-rt-blas-portable-floor.md)

## Goal

Turn Phase 7's single portable BLAS build into a **`displaced-geometry → BLAS` seam** with two
implementations behind it: the Phase-7 portable indirect / worst-case-degenerate-pad build as the
always-present **default**, and `VK_NV_cluster_acceleration_structure` (CLAS + cluster templates +
partitioned TLAS) as a **runtime-detected NVIDIA fast path** built from the *same* transient tessellated
vertex+index buffers Phase 4 emits. Raster and RT keep reading one identical geometry stream; the only
thing that changes on NVIDIA is *how* that stream becomes bottom-level acceleration structures. Nothing
NV-only is load-bearing — on llvmpipe / AMD / Intel the seam falls back to the portable build cleanly,
and the seam is shaped so a future KHR cluster-AS or the AMD+Samsung DGF compressed-geometry backend
slots in as a third implementation without touching the tessellator, the raster path, or the TLAS ring.

This is a pure optimization layered over the correctness floor. The Phase-7 portable path is the default,
is verified independently, and is never removed.

## Build plan

### 1. The `displaced-geometry → BLAS` seam (the abstraction Phase 7's build collapses into)

Phase 7 lands two concrete pieces in `rt.rs`: a `TessellatedBlas` cache type (distinct from `SkinnedBlas`
at `rt.rs:42`, forced to full `MODE_BUILD` every frame) and a per-frame planner that turns each
`DeformedRtInstance` (`draw_list.rs`) carrying a Phase-4 generated index stream into a build op recorded
by `record_tlas_build_plan` (`rt.rs:790`). Phase 8's first move is to **extract that build step into a
trait**, without changing behaviour:

```
trait DisplacedBlasBackend {
    /// Plan this frame's tessellated-BLAS builds over the transient VB/IB, returning ops the
    /// tlas-build pass records. Owns no &mut Rt state beyond its own cache.
    fn plan_builds(&mut self, device, dispatch, frame, instances: &[DeformedRtInstance],
                   transient_vb: vk::Buffer, transient_ib: vk::Buffer, counts: …) -> TessellatedBlasPlan;
    fn record(&self, raw, cmd, plan);
}
```

- The **default impl** (`PortableBlasBackend`) is exactly Phase 7's code: `AccelerationStructure::create`
  (`resources.rs:1094`) at the worst-case size, `vkCmdBuildAccelerationStructuresIndirectKHR` (probed
  `accelerationStructureIndirectBuild`, Phase 1) or the degenerate-pad worst-case `BUILD`, sized via
  `get_acceleration_structure_build_sizes` at `maxPrimitiveCount`, the per-build scratch pool, and the
  factor/LOD bucketing that lets same-bucket instances share one coarse `TessellatedBlas`. This is not
  new code — it is Phase 7's `plan_tessellated_blas_builds` moved behind the trait unchanged.
- The **CLAS impl** (`ClasBlasBackend`, this phase) is selected at `Rt::new` (`rt.rs:129`) when the
  device reports cluster-AS support (step 2), else the portable impl is chosen. The choice is a one-time
  `Box<dyn DisplacedBlasBackend>` field on `Rt` beside `dispatch` (`rt.rs:97`); every downstream site
  (`prepare_tlas_build` at `rt.rs:298`, the `tlas-build` graph pass in `renderer.rs ~3909`) calls the
  trait, not a concrete backend.

The TLAS ring is **untouched**. Both backends produce bottom-level structures the existing per-frame TLAS
full-rebuild (`prepare_tlas` at `rt.rs:514`, `instances_geometry` at `rt.rs:996`) references by device
address exactly as it references a `SkinnedBlas` today. The `AccelStructBuildRead` declared on the
transient resource by the `tlas-build` pass already orders the build after the Phase-4 emit pass — that
barrier derivation is reused verbatim for either backend (a CLAS build reads the same transient VB/IB).

### 2. Capability probe + extension enable in `device.rs` (mirror the mesh-shader probe)

The mesh-shader capability is probed and enabled in three coupled sites; CLAS mirrors each one exactly:

- **`Capabilities`** (`device.rs:80`): add `pub cluster_as_supported: bool` beside `mesh_shader_supported`
  (`device.rs:85`), with the same doc note that it never gates device selection.
- **`probe_optional_features`** (`device.rs:1046`): mirror the `mesh_shader_supported` block
  (`device.rs:1080`). CLAS **requires** `VK_KHR_acceleration_structure`, so gate on the already-computed
  `rt_supported` first, then `has_ext(VK_NV_cluster_acceleration_structure)` and a
  `PhysicalDeviceClusterAccelerationStructureFeaturesNV` query through
  `get_physical_device_features2` (the same push-next pattern as `as_feat`/`rq_feat` at `device.rs:1068`),
  reading its `cluster_acceleration_structure != 0`. Set the flag into the returned `Capabilities`
  (`device.rs:1097`).
- **`create_logical_device`** (`device.rs`): mirror `enable_mesh_shader` (`device.rs:1153`) with an
  `enable_cluster_as = enable_rt && has_ext(VK_NV_cluster_acceleration_structure)`; push the extension
  name into `device_extensions` beside the mesh-shader push (`device.rs:1167`) and chain a
  `PhysicalDeviceClusterAccelerationStructureFeaturesNV::default().cluster_acceleration_structure(true)`
  into the `create_info.push_next` chain under the `if enable_cluster_as` branch (mirror `ms_feat` at
  `device.rs:1229`).
- **`Device`** (`device.rs:130`): add a `cluster_as: Option<…::Device>` dispatch field beside
  `mesh_shader` (`device.rs:159`), resolved in the constructor when `capabilities.cluster_as_supported`
  (mirror the `mesh_shader` resolution at `device.rs:255`), plus `cluster_as_dispatch()` /
  `cluster_as_supported()` accessors mirroring `mesh_shader_dispatch()` (`device.rs:380`) and
  `mesh_shader_supported()` (`device.rs:386`).

**ash-version caveat (a real, load-bearing constraint, not a footnote).** The workspace pins
`ash = "=0.38"` (`Cargo.toml:19`), which resolves to `ash 0.38.0+1.3.281` — Vulkan-Headers **1.3.281**.
`VK_NV_cluster_acceleration_structure` first appeared in the 1.4.30x headers (early 2025), so this ash
build almost certainly exposes **no** `ash::nv::cluster_acceleration_structure` module, no
`PhysicalDeviceClusterAccelerationStructureFeaturesNV`, and no `cmd_build_cluster_acceleration_structure_indirect_nv`.
Phase 8 must resolve this one of two ways, and the modern-correct choice is the first:

1. **Bump `ash`** to a `0.38.x+1.4.30x` (or later) patch that carries the NV cluster-AS bindings, pin it
   once in `[workspace.dependencies]`, and use `ash::nv::cluster_acceleration_structure::Device` as the
   dispatch — a direct mirror of `ash::ext::mesh_shader::Device`. Re-run the gate; the header bump is
   additive (the extension list and every existing `vk::*` are a strict superset).
2. **Only if a bump is infeasible** (an incompatible transitive change): declare the handful of NV entry
   points (`vkGetClusterAccelerationStructureBuildSizesNV`,
   `vkCmdBuildClusterAccelerationStructureIndirectNV`) and the feature/properties structs behind a thin
   FFI shim in `rt.rs`. `saffron-rendering` already sets `#![allow(unsafe_code)]` crate-wide because ash
   is the FFI seam (`lib.rs:10-16`), so the shim lives inside the existing exception — no new
   crate-policy change. This is the fallback, not the plan.

Pick option 1 in the same change; do not leave option 2 as a lingering hand-rolled path.

### 3. The CLAS backend over the *same* transient buffers

`ClasBlasBackend::plan_builds` consumes the identical Phase-4 outputs the portable backend and the raster
passes consume — the transient vertex buffer (48 B `Vertex`, position@0 `R32G32B32_SFLOAT`, `types.rs`)
and the generated index buffer, both acquired via the Phase-1 keyed `TransientResources::acquire_buffer`
with the full usage union (`STORAGE|VERTEX|INDEX|INDIRECT|SHADER_DEVICE_ADDRESS|ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR`).
It never re-dices and never reads the base mesh — that is what keeps raster and RT one stream.

- **Clusters = dicing tiles.** The tessellator dices each base triangle into a clamped-parallelogram grid
  (Phase 4). That per-base-triangle grid **is** the natural cluster: partition the emitted index stream
  into ≤ 128–256-triangle CLAS along the per-base-triangle tile boundaries the Phase-3 predict/prefix-sum
  pass already knows (it computed each base triangle's exact output vertex/index counts and write
  offsets). No separate re-clustering of the emitted buffer is needed for the common case; the CLAS
  boundaries fall on tile boundaries, so a CLAS never straddles two base triangles and the per-CLAS
  vertex/index ranges are a direct slice of the transient buffers by the Phase-3 offsets.
- **Cluster templates keyed by (base-topology, LOD bucket).** A cluster template precomputes all
  position-independent build work; per frame only new vertex positions are supplied. Because the dicing
  **topology** for a tile is a pure function of its three edge factors, and Phase 3 already quantizes
  per-instance factors into **LOD buckets**, a template is built **once per (base-triangle-shape, LOD
  bucket)** and reused across every instance in that bucket and every frame the bucket is stable — the
  same bucketing that lets the portable backend share a coarse BLAS lets CLAS share a template. Cache
  templates in the `ClasBlasBackend` keyed by the LOD bucket id Phase 3 emits; invalidate a template only
  when its bucket's topology changes (the same reactive-resize trigger the portable AS uses).
- **GPU-driven indirect build.** `vkCmdBuildClusterAccelerationStructureIndirectNV` sources cluster
  counts and per-cluster inputs from device memory in one multi-indirect call — so on NVIDIA the CLAS
  path reads the **GPU-written primitive count directly** and needs **no** worst-case degenerate padding
  at all (it natively closes the GPU-count→CPU-build gap that forces the portable floor's indirect-or-pad
  handling). Record it in the `tlas-build` pass body beside where the portable backend records
  `cmd_build_acceleration_structures` (`rt.rs:834`); the cluster BLAS references its CLAS by device
  address, then the per-frame TLAS references the cluster BLAS — the existing
  `accel_build_to_build_read_barrier` / `accel_build_to_fragment_barrier` chain (`rt.rs:840/863`) orders
  CLAS→BLAS→TLAS→fragment exactly as it orders BLAS→TLAS today.
- **Partitioned TLAS (optional within this phase).** `VK_NV_partitioned_acceleration_structure` lets only
  changed tiles rebuild. It is a further optimization on top of CLAS and shares the same detect-or-fall-back
  discipline; land it only if the per-frame full TLAS rebuild (`prepare_tlas`) becomes a measured
  bottleneck under a heavy displaced-instance budget. Not required for the phase to be COMPLETED.

### 4. Runtime selection + the always-present portable fallback

- Selection is one branch at `Rt::new`: `cluster_as_supported()` → `ClasBlasBackend`, else
  `PortableBlasBackend`. Emit an info log mirroring the RT / mesh-shader lines
  (`device.rs:1344`, `renderer.rs:825`): `cluster acceleration structures available — RT displacement
  uses the CLAS fast path`.
- **Escape hatch for parity testing**, mirroring the `SAFFRON_MESH_SHADER` opt-in at `renderer.rs:822`:
  read `SAFFRON_RT_PORTABLE` in the selection branch and, when set, force `PortableBlasBackend` **even on
  NVIDIA**. This is the lever that makes the fix-#21 A/B silhouette comparison a single-flag toggle on one
  machine (CLAS frame vs portable frame of the same displaced rock), and it is the manual override the
  verification below leans on. Unlike `SAFFRON_MESH_SHADER`, the *default on NV is CLAS enabled* — the
  portable path is the escape hatch, not the reverse, because the portable path is already the proven
  floor from Phase 7.

### 5. Optional clusterer upgrade in `meshlet.rs`

`build_meshlets` (`meshlet.rs:180`) is a greedy linear per-submesh fill with only a bounding sphere per
`Meshlet` (`meshlet.rs:33`, 32 B, `#[repr(C)]` Pod). Optionally upgrade it to carry, per cluster: a
**normal cone** (for backface cluster cull in both the CLAS batching and the C2 task shader),
**edge-adjacency** (so neighbouring clusters agree on shared-edge dicing granularity), and a **coarse
LOD** level to drive dicing granularity. This feeds *both* the CLAS batching (better cluster shape/size)
and the C2 task shader (`meshlet.slang`) uniformly.

- Adding cone + adjacency + LOD fields **breaks the 32 B invariant** and the
  `meshlet_struct_is_32_bytes` test (`meshlet.rs:256`). Widen `Meshlet` deliberately (e.g. to 48/64 B),
  update that test's asserted size, and update `Uploader::upload_meshlet_buffers` (`upload.rs:1484`) which
  uploads the descriptor array verbatim — the stride change propagates through `MeshletBuffers`
  (`resources.rs:845`) automatically since it is `size_of::<Meshlet>()`-driven.
- This is **strictly optional** and gated the same way the rest of C2 is (`mesh_shader_supported()` for
  the task-shader consumer; cluster-AS support for the CLAS consumer). The greedy clusterer is adequate
  for the CLAS-from-dicing-tiles path in step 3 (which clusters the *emitted* tessellated buffer, not the
  base mesh), so this upgrade is a quality lever, not a prerequisite. Land it only if cluster culling or
  adjacency-driven dicing granularity is measured to help.

## Watertightness / RT / normal correctness

CLAS inherits watertightness — it does **not** re-derive it. The clusters partition the *same* welded,
seam-consistent triangles Phase 4 emits: a shared edge between two dicing tiles was already made
crack-free by Phase 4's weld pass (identical fractional factor from endpoints, identical displacement
value incl. UV-seam value agreement from Phase 2, identical direction, welded tangent). A CLAS boundary is
therefore **not a new seam** — it is a cut through already-agreeing geometry, so splitting the emitted
buffer into clusters cannot introduce a crack the portable single-BLAS build would not also have. The one
new invariant to enforce is that a cluster boundary falls on a *triangle* boundary (never mid-triangle),
which the step-3 tile-aligned partition guarantees.

Normals and tangents are untouched: CLAS references the same 48 B `Vertex` stream (position fetched from
the transient VB at offset 0; tangent@32 and the recomputed full-Jacobian normal/tangent frame from Phase
4 read by the fragment / closest-hit shader unchanged). The ray-traced surface is byte-identical geometry
to the rasterized surface under either backend, which is the whole point — the CLAS silhouette must equal
the portable silhouette because both are built over the exact same triangles.

## Verification (the fix-#21 acceptance)

- **CPU unit test (runs everywhere, incl. this GPU-less toolbox):** the seam *selection* is
  unit-testable. Drive `Rt`'s backend choice from a mocked `Capabilities` — `cluster_as_supported=false`
  must select `PortableBlasBackend`, `=true` selects `ClasBlasBackend`, and `SAFFRON_RT_PORTABLE` set
  forces portable regardless of the flag. This proves "nothing NV-only is load-bearing" and the clean
  fallback **without a GPU**, which is exactly what fix #21 asks be testable where the GPU path can't run.
  Add it beside Phase 7's build-path-selection tests.
- **Build + clippy gate:** `just engine` then `just prepare-for-commit` clean, including the ash bump's
  additive header surface. On llvmpipe (this environment) `cluster_as_supported` is `false`, the CLAS
  code path is never entered, and the present-only smoke + e2e stay green through the portable floor — a
  zero-risk change to the shipped default exactly as C2's mesh-shader path is.
- **GPU-with-eyes on the NVIDIA card (the real proof):** run the displaced-rock scene twice —
  `just run-engine` (CLAS, default on NV) and `SAFFRON_RT_PORTABLE=1 just run-engine` (portable) — and
  confirm the RT shadow **and** reflection silhouette of the displaced surface is **identical** between
  the two: same bulge, no self-shadow acne, no cracks at cluster boundaries, no flat mirror. A frame
  capture / screenshot A/B is the artifact; the validation layer must be clean in both runs (use the
  `nvidia_icd` macro / `just run-engine` so the NVIDIA device enumerates — llvmpipe would silently drop
  CLAS). Because the geometry is one stream, a mismatch is a CLAS clustering/build bug, not a content
  difference.

## Risks

- **`VK_NV_cluster_acceleration_structure` post-dates the pinned ash headers** (`ash 0.38.0+1.3.281`,
  headers 1.3.281). The bindings, feature struct, and build command are almost certainly absent, so the
  phase *starts* with an ash bump (or, worst case, an FFI shim behind the crate's existing
  `#![allow(unsafe_code)]` seam). If the bump drags in an incompatible transitive `vk::*` change the blast
  radius is wider than the RT crate — verify the bump in isolation via a private `CARGO_TARGET_DIR` before
  landing.
- **NV-only, single-vendor, still-evolving extension.** Nothing here may become load-bearing: the portable
  Phase-7 floor stays the default on every non-NVIDIA device and the `SAFFRON_RT_PORTABLE` override keeps
  it reachable on NVIDIA too. The seam is designed so a future KHR cluster-AS or the AMD+Samsung DGF
  backend is a third `DisplacedBlasBackend` impl — but that generality is unproven until such a backend
  exists, so keep the trait minimal (plan + record over transient VB/IB) and resist NV-specific leakage
  into the trait signature.
- **Cluster templates assume dicing topology is a pure function of the LOD bucket.** If Phase 3's
  bucketing does not actually pin per-tile topology (e.g. inner-factor derivation varies within a bucket),
  a template keyed by bucket is invalid and templates must be keyed more finely (per base-triangle-shape ×
  factor triple), eroding the reuse win. Confirm the Phase-3 bucket→topology invariant before caching
  templates by bucket; fall back to per-frame template rebuild if it does not hold.
- **CLAS clustering-from-tiles depends on Phase 3/4 emitting tile-aligned offsets.** If the prefix-sum
  packing interleaves output across base triangles for compaction, a CLAS could straddle a base-triangle
  boundary and the "boundary is not a new seam" argument weakens. Require the Phase-4 emit to keep each
  base triangle's output contiguous (it already writes at the Phase-3 per-triangle offsets), and assert
  cluster boundaries land on triangle boundaries.
- **Verifiability is environment-limited.** The toolbox has no NVIDIA card in CI and llvmpipe never
  reports cluster-AS support, so the CLAS *build/trace* path can only be proven on the NVIDIA host by hand
  (the GPU-with-eyes A/B). CPU tests prove selection and fallback; the silhouette-parity claim is not
  proven until run on the card. Do not mark the phase COMPLETED on the strength of the CPU tests alone —
  the fix-#21 "identical silhouette on NVIDIA" signal requires the card.
- **A widened `Meshlet` touches the C2 upload path.** The optional clusterer upgrade changes
  `size_of::<Meshlet>()`, so `upload_meshlet_buffers`, `MeshletBuffers`, and the `meshlet.slang` struct
  layout must move together in one change (NO-LEGACY: no second descriptor format). Skip the upgrade
  entirely if CLAS-from-dicing-tiles is sufficient, rather than half-widening the descriptor.
