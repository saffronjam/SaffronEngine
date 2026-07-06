# Real in-scene displacement — the tessellating, RT-correct mechanism

**Status:** NOT STARTED

One in-scene displacement mechanism: a compute **adaptive-tessellation** stage that *amplifies* base
triangles — adds vertices **and** emits a generated index stream — into per-frame `TransientResources`
vertex+index buffers, from which **both** the seven raster geometry passes (via indirect draw) and the
**RT BLAS** read one identical geometry stream. This compute→buffer spine is the only design that is at
once **cross-vendor** (pure compute + storage/indirect + core `VK_KHR_acceleration_structure`, identical
on AMD / Intel / NVIDIA / llvmpipe) and **RT-correct** (the geometry rays trace *is* the geometry
rasterized, so raster/RT silhouette divergence and self-shadow acne cannot exist).

`HeightMode::{Bump | Parallax | Displacement}` stays the material-facing vocabulary — three distinct
correct tiers, not duplicates. Parallax is the **default** for an imported height map; **Displacement**
is a deliberate authored choice that carries a factor budget, so the flat / low-poly common case never
silently pays tessellation + a per-frame BLAS build.

## The motivating problem

`HeightMode::Displacement` today (`crates/rendering/src/displacement.rs`, `assets/shaders/displace.slang`)
runs **one thread per base vertex**: it moves each existing vertex along its normal by `height * scale`
into the shared `Skinning` deformed-vertex ring and writes `oBase = (deformedOffset + i) * 48` — a strict
1:1 bijection. It **adds no geometry**. Triangle connectivity is invariant; the index buffer is never
produced or touched; scene passes and the BLAS reuse `mesh.index_buffer()` verbatim. So displaced
silhouette detail is bounded entirely by the base mesh's authored density.

The codebase works around this with a crutch: the material previewer swaps in a bespoke ~56k-vertex dense
sphere (`preview_displacement_sphere`, `PREVIEW_DISPLACE_SPHERE_MESH_ID = Uuid(7)`, seeded at
`load.rs:255`) and *forces* `HeightMode::Displacement` at a magic `height_scale = 0.08`
(`commands_asset.rs:3046-3053`) so the preview bulges. A real low-poly **scene** mesh has no such dense
stand-in, so its displaced result reads like a bump — the preview and the scene structurally diverge.

The requirement: **real displacement on arbitrary, low-poly scene meshes** — new vertices at displaced
positions, a true silhouette, visible to every shadow map **and to the ray-traced BVH** (shadows,
reflections, AO, GI, ReSTIR). That is exactly what the current 1:1 kernel cannot do and what forces the
whole tessellate-into-a-buffer-that-also-feeds-the-BLAS design. Reaching the BVH is the hard, load-bearing
part: mesh-shader or fixed-function tessellator output never materializes into a buffer the AS builder can
read, so any raster-only tessellator re-introduces the raster/RT divergence this planset exists to
eliminate.

## Research verdict

| Technique | Verdict | Reason |
|---|---|---|
| **Compute adaptive tessellation → transient VB/IB → BLAS** | **ADOPT — the spine** | Only path that is simultaneously cross-vendor (compute + storage/indirect + core `VK_KHR_acceleration_structure`) and RT-native (output is an ordinary VB/IB the BLAS build consumes directly). Rays trace the geometry that was rasterized. |
| Watertight shared per-edge **fractional** factors (DiagSplit) + seam weld + **UV-seam VALUE agreement** | **ADOPT — mandatory** | Watertightness is a construction property, not a fix-up. Break any link (factor, value, direction, tangent, base weld) and RT silhouettes leak light or crack. |
| Clamped-parallelogram dicing + Phong base + **full-Jacobian** normals/tangents | **ADOPT** | Graceful matched-gap stitch, pop-free; one pinned edge-continuous smoothed base; normals **and** tangents re-derived from the full position Jacobian (base curvature + displacement derivative). |
| Scalar-along-normal **and** tangent-space **vector** displacement | **ADOPT** | Both route through the existing bindless `DisplaceInfo.height_index`/`vector_index` sampling (B3). Vector handles overhangs. |
| **Identity-free** motion vectors + geomorph across integer factor transitions | **ADOPT — mandatory for TAA** | Re-dicing changes vertex count/identity every frame, so there is no per-vertex history; reconstruct previous clip position from the same barycentric sample vs the previous transform + height. |
| Indirect AS build / **degenerate-pad worst-case** floor | **ADOPT — the RT floor** | `vkCmdBuildAccelerationStructuresIndirectKHR` gated on the probed `accelerationStructureIndirectBuild`, else build at CPU worst-case `maxPrimitiveCount` and pad unused micro-tris to degenerate triangles. Portable everywhere. |
| Factor/LOD **BLAS bucketing** + hard displaced-instance budget | **ADOPT — mandatory for populated scenes** | Screen-space factors make hardware instancing impossible; without shared coarse per-bucket BLAS + a budget, a forest of displaced instances is infeasible. |
| `VK_NV_cluster_acceleration_structure` (CLAS) | **ADOPT — behind the seam, NV fast path** | Detected NVIDIA build backend over the *same* transient buffers; never load-bearing. Portable floor stays the default. |
| Analytic-prism / projective-displacement `VK_KHR_ray_query` | **ADOPT — editor satellite only** | Zero-rebuild live-edit RT preview; **APPROXIMATE during a drag, EXACT on commit** (the baked diced BLAS). Never the shipping default; never claimed bit-identical. |
| `VK_KHR_opacity_micromap` (cross-vendor as of 1.4.351) | **ADOPT — optional, non-gating** | Cuts any-hit cost on alpha-cut displaced detail. Must not gate COMPLETED; add only when alpha-cut materials exist. |
| Mesh / task shaders (`VK_EXT_mesh_shader`) | **KEEP — raster-only front end** | The C2 front end stays an *optional raster consumer* of the one transient buffer — never a re-dicer, never the RT source. |
| Fixed-function tessellation (TCS/TES) | **SKIP** | No RT story (output never enters a BVH), factor-64 cap, 2×2-quad micro-tri overshading, uneven on Metal/mobile. The path UE5 deleted. The engine has no TESC/TESE stages. |
| `VK_NV_displacement_micromap` (DMM) | **SKIP** | Deprecated — SDK/Toolkit archived Jan 2025, driver support removed, never promoted to KHR, NV-Ada-only, static/baked. Ideas donor only (prism min/max, barycentric encoding). |
| Nanite wholesale (cluster-DAG + visibility-buffer SW raster) | **SKIP — ideas only** | Bespoke pipeline (software raster, streaming, DAG bake), not a portable Vulkan primitive. Lift amplify-in-pipeline, heightfield-as-data, conservative bounds, screen-space error; not the machinery. |
| Parallax occlusion mapping as the displacement / RT source | **SKIP as source; KEEP as far-LOD tier** | Flat silhouette, invisible to secondary rays. Stays the flat-detail tier under `HeightMode` with the D3 offset-limiting march, and is now the auto-routed default for imported height maps. |

## Chosen stack

**Spine (build on these):**

- Compute adaptive tessellation that amplifies base triangles into per-frame `TransientResources` VB/IB.
- **Fractional** shared per-edge factors from endpoints only (DiagSplit local-edge invariant), with a
  visibility-robust world-space fallback for clipped / behind-near / back-facing endpoints so both
  incident triangles agree regardless of on-screen state.
- Clamped-parallelogram dicing + a specified matched-gap stitch triangulation over **one** pinned
  edge-continuous smoothed base (Phong tessellation, edge curve from endpoints only).
- Normals **and** tangents re-derived from the full position Jacobian (full 3×3 for vector displacement).
- Import-time spatial-hash **seam weld** with a per-welded-vertex displacement direction **and** a
  seam-consistent tangent, plus **UV-seam height VALUE agreement** (seam-aware dilation with a verified
  texel match, or object-space/triplanar sampling flagged per seam edge).
- Scalar-along-normal **and** tangent-space vector displacement via the existing bindless sampling.
- Identity-free previous-position reconstruction for motion vectors + geomorph across integer transitions.
- Predict → prefix-sum → indirect-args allocation with a worst-case reserve + GPU-exact packing;
  `cmd_draw_indexed_indirect(_count)` raster.
- RT floor: `vkCmdBuildAccelerationStructuresIndirectKHR` (probed) or a degenerate-padded CPU worst-case
  build; worst-case-sized AS + scratch with a per-build scratch pool; factor/LOD BLAS bucketing + a
  displaced-instance budget.

**Capability / edit seams (behind a `displaced-geometry → BLAS` abstraction, never load-bearing):**
`VK_NV_cluster_acceleration_structure` as a detected NVIDIA build backend; the analytic-prism
`VK_KHR_ray_query` intersection path as the editor-only approximate live-edit preview (exact on commit);
`VK_KHR_opacity_micromap` strictly optional and non-gating.

### The three make-or-break seams, spelled out

These were the under-specified gaps in `plans/displacement/`. They are now first-class, each owned by a
phase:

1. **GPU primitive count → a CPU-sized build.** `get_acceleration_structure_build_sizes` needs
   `maxPrimitiveCount` at record time and the CPU range struct needs a count, but the count is decided on
   the GPU. Floor: `vkCmdBuildAccelerationStructuresIndirectKHR` gated on the probed
   `accelerationStructureIndirectBuild`, else build at CPU worst-case and pad unused micro-tris to
   degenerate triangles. AS + scratch sized to the per-instance **worst-case** bound (reactive resize only
   when that bound changes, never on per-frame factor wobble); a per-build scratch pool / batched
   multi-geometry builds keep per-instance BUILDs overlapping. **Owner: Phase 7** (probe in Phase 1).
2. **Temporal stability.** Re-dicing destroys per-vertex history, so reconstruct previous clip position
   identity-free: re-evaluate the same barycentric sample against the previous instance transform +
   previous height (previous factors/transform double-buffered). Fractional per-edge factors + geomorph
   across integer transitions kill dolly/zoom pop; a geomorph is smooth so it does not spike motion
   vectors. **Owner: Phase 5** (producer in Phase 4).
3. **UV-seam height VALUE agreement.** At a UV seam the two incident triangles carry different UVs;
   watertightness needs them to sample an **equal** height value — resolved at import by seam-aware
   dilation with a verified texel match, or a per-seam-edge object-space/triplanar sampling flag.
   **Owner: Phase 2** (consumed in Phase 4).

## Material-facing mode vocabulary

One height/vector map slot + `HeightMode {Bump | Parallax | Displacement}` (`core/src/height.rs`, the
frozen `.smat` `heightMode`/`heightScale`/`height`+`vectorDisplacement` wire — unchanged). Displacement's
*implementation* changes underneath; the vocabulary does not. `detect_height_mode` (`scan.rs`) is
**re-pinned** so an imported height map defaults to **Parallax** — Displacement is opted into deliberately
and carries a factor budget. One world-space `height_scale` amplitude convention is shared by preview and
scene, pinned in Phase 4 before the forced-`0.08` preview crutch is deleted in Phase 6.

## Phase ordering + dependency DAG

Nine phases. DAG: **1→3, 2→3, 3→4, 4→5, {4,5}→6, 4→7, 7→8, {2,7}→9.** Phases 1 and 2 are independent
foundations; 3 needs both; 4 is the amplifying kernel and the NO-LEGACY cutover; 5 and 6 consume it for
raster; 7 is the portable RT floor; 8 is the NV fast path; 9 adds the editor satellite, the control
surface, docs, and retires the old planset.

- **[`phase-1-graph-indirect-foundations.md`](phase-1-graph-indirect-foundations.md)** — graph vocabulary
  (`RgUsage::IndexInputRead`, `IndirectCommandRead`), indirect dispatch/draw plumbing, capability probes
  (`drawIndirectCount`, `multiDrawIndirect`, `accelerationStructureIndirectBuild`), and the **keyed**
  `TransientResources::acquire_buffer` variant — all with zero behaviour change so the gate stays green.
- **[`phase-2-import-watertight-conditioning.md`](phase-2-import-watertight-conditioning.md)** —
  import-time edge adjacency, spatial-hash weld with per-welded-vertex direction + seam-consistent
  tangent, **UV-seam height value agreement**, and a per-height-texture min-max pyramid. Self-verifiable
  CPU tests, no dependence on the later dicer.
- **[`phase-3-edge-factors-allocation.md`](phase-3-edge-factors-allocation.md)** — visibility-robust
  fractional per-edge factors, predict→prefix-sum→indirect-args worst-case allocation (first
  `TransientResources` consumer), factor cap / global budget / LOD bucketing + a shrink/reclaim path.
  *Depends on 1, 2.*
- **[`phase-4-dice-displace-weld-emit.md`](phase-4-dice-displace-weld-emit.md)** — the amplifying
  dice+displace+weld+emit kernel (full Jacobian, skinned-base input, identity-free prev-position
  producer) and the NO-LEGACY retirement of the 1:1 kernel + the `deformed_cursor += vertex_count`
  reservation. *Depends on 3.*
- **[`phase-5-temporal-motion-vectors.md`](phase-5-temporal-motion-vectors.md)** — identity-free motion
  vectors + geomorph continuity, so TAA does not ghost displaced silhouettes. *Depends on 4.*
- **[`phase-6-raster-indirect-consumption.md`](phase-6-raster-indirect-consumption.md)** — all seven
  geometry passes read the generated index buffer via `cmd_draw_indexed_indirect(_count)`; re-pin the
  height-mode routing default; delete the preview-sphere crutch (preview == scene). *Depends on 4, 5.*
- **[`phase-7-rt-blas-portable-floor.md`](phase-7-rt-blas-portable-floor.md)** — indirect / degenerate-pad
  worst-case BLAS build, worst-case AS+scratch sizing, a scratch pool, instance bucketing/budget, and
  distinct `SkinnedBlas` (UPDATE) vs `TessellatedBlas` (BUILD) policies. *Depends on 4.*
- **[`phase-8-nv-clas-fast-path.md`](phase-8-nv-clas-fast-path.md)** — `VK_NV_cluster_acceleration_structure`
  as a detected fast path behind the `displaced-geometry → BLAS` seam over the same buffers. *Depends on 7.*
- **[`phase-9-editor-prism-and-polish.md`](phase-9-editor-prism-and-polish.md)** — editor-only
  analytic-prism `VK_KHR_ray_query` preview (approximate on drag, exact on commit), the `sa` tessellation
  control command + e2e, the docs page, and the deletion of `plans/displacement/`. *Depends on 2, 7.*

## Relationship to `plans/displacement/`

This planset **supersedes** `plans/displacement/`. On landing (Phase 9) that folder is **deleted** — it is
removable only once COMPLETED, so the old phase files stay until the cutover lands, then go. The old
planset's B1/B3/C1/C2/D infrastructure is the foundation; its explicitly-deferred hard problems (adaptive
tessellation + watertight welding) plus the newly-surfaced ones (UV-seam value agreement, motion-vector
identity, GPU-count-into-a-CPU-build, instancing blow-up) are what this planset pays down.

### Reuse / adapt (built infrastructure this planset depends on)

- **D-track (D1/D2/D3):** the `HeightMode {Bump|Parallax|Displacement}` enum (`core/src/height.rs`), the
  frozen `.smat` `heightMode`/`heightScale`/`height`+`vectorDisplacement` wire, the
  `FEATURE_HEIGHT_BUMP`/`FEATURE_HEIGHT`/`FEATURE_DISPLACE` resolve bits, the offset-limiting `parallaxUv`
  march. **Re-pinned:** `detect_height_mode` defaults an imported height map to Parallax (Phase 6). The
  vocabulary is unchanged; Displacement's implementation changes underneath.
- **B3 (vector displacement + 48 B tangent `Vertex`):** `Vertex` tangent@32, `compute_tangents`,
  `MESH_FORMAT_VERSION`. The tangent frame is the per-welded-vertex direction **and** the seed for the
  recomputed seam-consistent tangent basis.
- **B1 (`TransientResources`):** the dormant grow-only pool (`transient.rs`) becomes its first consumer,
  **upgraded** with a keyed acquire variant (Phase 1, so a conditionally-present tessellation pass — or the
  Phase-9 prism, a second consumer — never desyncs the cursor) and a shrink/reclaim path (Phase 3, to
  bound session-peak VRAM under grow-only).
- **C2 (mesh-shader front end):** kept as an **optional raster-only** consumer of the transient buffer —
  never the RT source, never a re-dicer.
- **C1 principle + the entity-keyed BLAS map + per-frame TLAS ring (`rt.rs`):** survive; the topology/index
  source and refit-vs-rebuild policy change, and the BLAS cache **splits** into distinct `SkinnedBlas`
  (UPDATE) and `TessellatedBlas` (BUILD) types.
- **The render-graph deform scope** (`RgPass::compute` in `record_scene_graph`): tessellation passes slot
  in identically; it also hosts the skinning→tessellation ordering for skinned+displaced.

### Retire / replace (deleted in the same change that lands the replacement — NO-LEGACY)

- **B2 baseline:** `displace.slang` computeMain's 1:1 kernel and `Displacement::wire_dispatches`' reuse of
  the `Skinning` deformed ring for Displacement-mode → the amplifying tessellator writing
  `TransientResources` (Phase 4).
- **`deformed_cursor += vertex_count`** fixed reservation for Displacement (`instancing.rs:330-451`) →
  worst-case / counter-driven transient allocation with a generated index buffer (Phases 3, 4).
- **Static-index draw path** for displaced batches (`scene_pass.rs` hardcoded `batch.mesh.index_buffer()`,
  `deformed_vertex_offset + submesh.vertex_offset`) and the base-index source in
  `rt.rs:plan_skinned_blas_refits` → generated index buffer + `cmd_draw_indexed_indirect` + indirect /
  worst-case BUILD (Phases 6, 7).
- **In-place MODE_UPDATE refit for tessellated instances** → a per-instance policy on distinct types
  (UPDATE for skinning/morph; forced BUILD for tessellated, incl. skinned+displaced) (Phase 7).
- **Preview crutches:** `preview_displacement_sphere` / `PREVIEW_DISPLACE_SPHERE_MESH_ID` (Uuid 7) +
  `load.rs:255` seeding, and the forced `HeightMode::Displacement` / magic `0.08` in
  `preview_material_for_texture` → preview honors the map's real (re-pinned) mode at the shared world-space
  amplitude (Phase 6).

## Reconstructed open questions

Written inline so the design is self-contained (the old planset referenced an external dossier):

- **Watertight edge factors incl. the visibility fallback** — identical fractional factor from endpoints
  only, and the world-space fallback when an endpoint is clipped / behind near / back-facing (Phase 3).
- **World-space amplitude** — one `height_scale` convention shared by preview and scene (Phase 4).
- **Per-frame cost vs caching** — static-cache where factors are temporally stable; hysteresis/clamp on
  edge factors to cut rebuild churn (Phases 3, 7).
- **Refit vs rebuild** — per-instance policy: UPDATE for fixed-topology deform, forced BUILD for variable
  topology (Phase 7).
- **GPU count into a CPU-sized build** — indirect build (probed) or degenerate-padded worst-case (Phase 7).
- **UV-seam VALUE agreement** — seam-aware dilation with a verified match, or object-space/triplanar seam
  sampling (Phase 2).
- **Motion-vector identity + pop** — identity-free reconstruction + geomorph (Phases 4, 5).
- **Instancing blow-up** — factor/LOD bucketing + a displaced-instance budget (Phases 3, 7).
- **Vector-field filtering** — avoid collapsing the offset direction under mip filtering (Phase 4).
- **PSO-cache interaction** — the tessellation permutation vs the übershader/PSO cache (Phases 4, 6).
- **Projective-displacement maturity** — approximate prism march scoped to the editor satellite (Phase 9).
- **Shadow-LOD-from-main-camera compromise** — factors are main-camera-derived and shared across all seven
  passes; an optional endpoint-only light-proximity term, else the compromise is documented (Phase 3).
- **Cross-vendor RT verifiability** — the floor is architected portable but proven only where a GPU exists
  (see gating).

## Verifiability + gating

- **CPU unit tests only** (no GPU needed): Phase 1 (graph rows, keyed-acquire, indirect round-trip),
  Phase 2 (adjacency / weld / pyramid / seam-agree), Phase 3 (worst-case sizing, exact packing, shared
  fractional factor incl. off-screen endpoint), Phase 5 (remainder→geomorph-weight mapping), Phase 7
  (build-path selection, worst-case sizing math, bucketing, UPDATE-vs-BUILD policy per instance type).
- **Needs a GPU** for visual parity: Phases 4, 5, 6 (crack-free shared edge, welded tangent continuity,
  no TAA ghosting, true bulged silhouette, preview == scene), Phase 7 (RT silhouette == raster silhouette),
  Phase 8 (CLAS == portable silhouette on NVIDIA).
- **Deferred, not proven:** real-GPU cross-vendor validation on AMD/Intel — no hardware GPU in the toolbox
  and limited/slow lavapipe RT. The floor is architected to be portable and nothing NV-only is
  load-bearing, but it is **not claimed proven** until it can be run.

## Acceptance signals

- Preview == scene for the raster/baked path (the dense-sphere + forced-`0.08` crutches deleted).
- RT shadow/reflection silhouette == raster silhouette on the available GPU (no self-shadow acne, no flat
  mirror).
- No TAA ghosting/smearing on displaced silhouettes; no motion-vector discontinuity across a factor
  transition.
- An ordinary imported height map stays Parallax (no tessellation, no BLAS build).
- `just check` clean; an e2e control-driven tessellation-budget command with a validation-clean log.

## Key sources

- **Compute-tessellation spine:** Filmic Worlds "Adaptive Compute Tessellation" (~0.46 ms adaptive vs
  ~14.8 ms fixed factor-127; the worked predict→scan→plan→emit→weld pipeline) and "Compute Tessellation
  with Clamped Parallelograms"; GPU Zen 2 "Adaptive GPU Tessellation with Compute Shaders"
  (Khoury/Dupuy/Riccio).
- **Watertightness:** Stanford DiagSplit (local-edge-factor invariant → crack-free parallel dicing);
  MikkTSpace tangents (glTF cross-tool standard); spatial-hash seam welding.
- **Conforming adaptive subdivision (ideas):** Dupuy Concurrent Binary Trees + longest-edge bisection;
  Dupuy & Benyoub "CBT for Large-Scale Game Components" (arXiv 2407.02215).
- **RT displacement:** Projective Displacement Mapping (arXiv 2502.02011) and TFDM (Thonat/Boubekeur,
  SIGGRAPH Asia 2021) for the analytic-prism min-max-pyramid march; DJM triangle-Jacobian base meshes
  (arXiv 2606.22880) as an ideas donor.
- **Vulkan RT primitives:** `VK_KHR_acceleration_structure` indirect build
  (`vkCmdBuildAccelerationStructuresIndirectKHR` + `accelerationStructureIndirectBuild`); RTX Mega
  Geometry / `VK_NV_cluster_acceleration_structure` (nvpro `vk_tessellated_clusters`, `vk_lod_clusters`);
  `VK_KHR_opacity_micromap` (promoted cross-vendor in Vulkan 1.4.351).
- **Rejected-for-cause:** UE5 Nanite Tessellation (graphicrants, Karis SIGGRAPH 2021 Advances) — ideas,
  not machinery; `VK_NV_displacement_micromap` (archived Jan 2025) — dead; fixed-function tessellation —
  the path UE5 deleted.
