# Phase 9 — Editor analytic-prism RT preview (approximate, exact-on-commit) + control surface + docs + retire old planset

**Status:** IN PROGRESS — the CPU/wire/docs keep-current obligations are landed and gated (workspace
clippy `-D warnings` clean; 322 protocol + 69 control tests pass; headless validation-clean on the RTX
3070 Ti):
- **§2 tessellation-quality control command** — `set-tessellation-quality [factorCap] [minFactor]
  [edgeLengthTarget]` (`SetTessellationQualityParams`/`Result` DTOs, `codegen`/`COMMANDS`/`DTO_TYPE_NAMES`/
  `render_domain`/`COMMAND_FIXTURES` wired + regenerated). Threads a runtime-tunable budget through
  `Renderer::set_tessellation_quality` → `DrawListInputs` → each `TessBucket` (replacing the hardcoded
  `TESS_DEFAULT_*`), clamped to `cap∈[1,64]`, `min∈[1,cap]`, `edge≥1`. Reachable from `sa` immediately
  (external-subcommand passthrough); the `tess-quality` schema-contract fixture covers the wire. Control
  unit test asserts the clamp + partial-update + readback; an e2e test (`tessellation-quality.test.ts`)
  drives it over the control plane.
- **§3 docs** — `compute-displacement.md` rewritten from the deleted 1:1 path to the shipped tessellation
  mechanism (dice→displace→weld→emit; raster indirect + `TessellatedBlas`; the quality command; the
  approximate prism satellite), and the frame-and-render-graph hub row updated.

**§5 DONE — `plans/displacement/` retired:** every phase of the old planset was verified against the
current tree (feature end-state present, or superseded by the strictly stronger tessellation spine),
marked COMPLETED, and the folder deleted.

**Remaining (GPU-visual / editor, need presenting hardware):** §1 the analytic-prism AABB-BLAS +
inline-ray-query march + editor live-edit preview mode; §4 the optional opacity-micromap companion.

**Scope:** `saffron-rendering` (a second AABB-geometry BLAS path + an inline-ray-query prism march + an
editor-preview render mode), `saffron-control` + `saffron-protocol` (the tessellation-quality command +
its DTOs + `render-stats` readback), `editor` (engage the prism mode during a live material-graph
displacement drag), `tests/e2e` (a control-driven budget test), `docs/` (the displacement concept page +
its hub row). Finally deletes `plans/displacement/` and marks this planset `COMPLETED`.

**Depends on:** [`phase-2-import-watertight-conditioning.md`](phase-2-import-watertight-conditioning.md)
(the per-height-texture min-max pyramid + the per-welded-vertex direction/seam data the prism march
samples), [`phase-7-rt-blas-portable-floor.md`](phase-7-rt-blas-portable-floor.md) (the baked diced
`TessellatedBlas` the preview is *exact against on commit*, and the `displaced-geometry → BLAS` seam the
prism sits beside). Also uses Phase 1's keyed `TransientResources::acquire_buffer` and capability-probe
scaffold, and Phase 3's budget knobs that the control command feeds.

## Goal

Close the planset with the one deliberately-approximate satellite the shipping spine does not provide,
and discharge the AGENTS.md keep-current obligations.

1. An **editor-only live-edit RT displacement preview**: a cross-vendor `VK_KHR_ray_query` inline
   analytic-prism march that keeps only coarse base prisms in an AABB BLAS and marches the Phase-2 min-max
   pyramid inside each prism, sharing the exact height field + sampling function with the baked path — so
   dragging an amplitude / editing a displacement node updates ray-traced shadows/reflections **with zero
   BLAS rebuild**. It is **APPROXIMATE during a drag and EXACT on commit** (the baked diced
   `TessellatedBlas` from Phases 4/7). It is **never** the shipping scene default, and "preview == scene"
   is a claim only for the raster/baked path, never for the prism drag.
2. A **`sa` control command for tessellation quality** — edge-length budget, per-instance factor cap,
   global micro-tri budget — feeding the Phase-3 budgets, with an e2e test driving it over the control
   plane and asserting a validation-clean log.
3. **Docs** — the displacement concept page, updated to the one mechanism + the `displaced-geometry →
   BLAS` seam + the approximate-preview satellite, and its hub `_index.md` row.
4. **Optional, non-gating**: a `VK_KHR_opacity_micromap` companion for alpha-cut displaced detail — no
   acceptance criteria, must not gate `COMPLETED`.
5. **Retire** `plans/displacement/` and mark this planset `COMPLETED`.

## Why an approximate satellite at all, and why it is safe

The shipping RT path (Phase 7) rebuilds a `TessellatedBlas` from the Phase-4 diced triangles. That is
correct and cross-vendor, but a per-edit rebuild on every amplitude wobble is exactly the churn a live
material-graph drag produces. The analytic prism (Projective/TFDM model) trades a per-ray software march
for **zero acceleration-structure rebuild on edit**: only a coarse per-triangle prism AABB is in the BVH,
and the height change is just a texture the march reads. This is the one place the planset accepts a
second RT representation, and it is walled off:

- **Editor only, live-edit only.** It engages only while a displacement value is being dragged in the
  material-graph editor's live preview; on commit the editor tears it down and the real diced
  `TessellatedBlas` takes over. It is never wired for a shipping scene, and the mode is unreachable from
  `saffron-player`.
- **Approximate by construction, and we say so.** The min-max-pyramid march does not discretize to the
  same triangles the Phase-4 dicer emits, so its silhouette is *not* bit-identical to the baked BLAS. Drop
  any "agrees exactly" language; the headline `preview == scene` acceptance applies to the raster/baked
  path (Phase 6) and the RT silhouette == raster silhouette check (Phase 7), not to the prism drag.
- **Cursor-safe.** As a *second* `TransientResources` consumer (after the Phase-3 tessellator) it uses
  Phase 1's keyed `acquire_buffer(frame, key, bytes, usage)` at a fixed label, acquiring zero bytes on
  frames where no drag is active, so it cannot desync the pool cursor.

## Build plan

### 1. The AABB-geometry BLAS for coarse base prisms (`saffron-rendering`, `rt.rs`)

`rt.rs` today builds only **triangle** geometry — `triangle_geometry` fixes `R32G32B32_SFLOAT` positions
+ `UINT32` indices by device address, consumed by `AccelerationStructure::create` /
`plan_skinned_blas_refits` / `record_mesh_blas_build`. The prism preview needs the other Vulkan geometry
kind: **AABBs** (`VkAccelerationStructureGeometryAabbsDataKHR`, one `VkAabbPositionsKHR` per primitive).

- Add a sibling `aabb_geometry(device_address, stride, count)` helper beside `triangle_geometry`,
  producing `vk::AccelerationStructureGeometryKHR` with `geometry_type = AABBS` and the non-opaque flag so
  the ray query yields **candidate AABB** intersections the shader resolves inline (an AABB BLAS is never
  opaque — the caller must commit the hit). Reuse `AccelerationStructure::create` + `ensure_blas_scratch`
  unchanged; only the geometry descriptor differs.
- **One prism AABB per coarse base triangle** (or per Phase-3 LOD-bucket coarse patch): the prism is the
  base triangle extruded along the Phase-2 **per-welded-vertex displacement directions** between the
  triangle's local `[min,max]` displaced height (from the min-max pyramid's coarsest level scaled by the
  material `height_scale`). The per-triangle prism-bound buffer (min/max height, the three welded
  directions, the base-vertex indices) is written once per subject by a small compute pass — or on the CPU
  for the single-material preview subject — into a **transient buffer** acquired via the keyed
  `acquire_buffer` with usage `STORAGE|SHADER_DEVICE_ADDRESS|ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR`
  (the same union `make_deformed_buffer` uses when RT is on). The AABB positions buffer is a second keyed
  slot.
- Build this prism BLAS with `MODE_BUILD` **once per subject** (topology is the coarse base mesh, which
  does not change during a drag). On an amplitude change only the per-prism `[min,max]` grows, so the AABB
  bounds are refit (or, given the tiny primitive count, rebuilt) — but critically **no bottom-level
  displacement AS is ever rebuilt from micro-triangles**. Reference the prism BLAS from a dedicated
  preview TLAS instance (reuse the `FrameRt.tlas` ring + `record_tlas_build_plan`, or a small dedicated
  TLAS for the preview view — the ring already full-rebuilds each frame and absorbs it).

### 2. The inline analytic-prism march (`saffron-rendering`, a new `prism_displace.slang` include)

The engine's RT is **inline ray query** (set-6 TLAS read from `lighting.slang` and
`restir_resolve.slang`), not a ray-tracing pipeline with an SBT. So the prism intersection is resolved
**inline in the ray-query loop**, not in a separate intersection stage: when `rayQueryGetIntersectionType`
returns an AABB candidate for a prism primitive, the shader marches the height field and, on a hit, calls
`rayQueryGenerateIntersection` to commit. This is pure cross-vendor `VK_KHR_ray_query` — already probed
and enabled (`device.rs` `has_rq` / `rt_supported`, `PhysicalDeviceRayQueryFeaturesKHR`), so no new
extension is needed for the march itself.

- **Shared height field + sampling.** Factor the height/vector sample used by `displace.slang`'s
  `computeMain` (bindless set-0 height/vector index, tiled UV, `height_scale`) into a shared Slang include
  that **both** the baked dice kernel (Phase 4) and the prism march call — so preview and baked path read
  one height function (the mandate: "sharing the exact height field + sampling function"). The march adds
  only the traversal: project the ray into the prism's texture space, step the Phase-2 **min-max pyramid**
  (max-mipmap) for conservative empty-space skipping, and converge on the displaced surface (the
  Projective/TFDM model — tight prism bounds, thin-feature sampling via a stochastic march start, smoothed
  displaced normals).
- **Normal correctness.** Emit the analytic height-gradient normal from the pyramid and apply the
  faceting-removal correction `N' = Ns − Ng + N_shading` (interpolated smooth base normal minus geometric
  normal plus the sampled bump) — the same shading normal convention the raster bump path uses, so the lit
  preview matches the raster surface in-the-small even though its silhouette is approximate.
- **Watertightness of the proxy.** Adjacent prisms share a base edge and, via the Phase-2 per-welded-vertex
  direction, the same extrusion direction on that edge, so their side walls coincide — the proxy is
  watertight-by-construction at prism boundaries (no light-leak seams), even though the *displaced hit* is
  approximate. UV-seam height **value** agreement (Phase 2) makes the shared height read equal on both
  sides.

### 3. The editor-preview render mode + editor wiring (`saffron-rendering` + `editor`)

- **A render mode, not a scene default.** Add a renderer flag (mirror `set_displacement` /
  `displacement_enabled` in `renderer.rs`) that, for the **`ViewId::AssetPreview`** view only, routes the
  displaced subject through the prism BLAS + inline march instead of the Phase-4 dice + `TessellatedBlas`.
  It is only ever set by the editor for the live-edit preview and is a no-op on the scene view and in
  `saffron-player`.
- **Reuse the shipped live-preview host path.** The material-graph live preview already
  renders the edited `.smat` on the orbitable sphere via `enter-asset-preview` + the modal `preview_scene`
  on `ViewId::AssetPreview`. This phase engages the prism mode on that exact subject while an amplitude /
  displacement-node value is being dragged, and disengages (falling back to the baked `TessellatedBlas`)
  on pointer-up / commit. **Approximate on drag, exact on commit** is literally this begin-drag →
  prism-mode → end-drag → bake-and-swap transition.
- **Keyed transient acquire, fixed order.** The prism's two transient buffers (prism-bound + AABB
  positions) are acquired every frame at stable keys via Phase 1's `acquire_buffer`, zero-size when no
  drag is active, so co-existing with the Phase-3 tessellator's slots never desyncs the pool cursor
  (the exact fragility Phase 1 exists to remove).

### 4. Control surface — the `sa` tessellation-quality command (keep-current rule)

A feature that adds engine state worth driving/inspecting gets a matching control command. The Phase-3
budgets (edge-length target, per-instance factor cap, global micro-tri budget) are that state.

- **One registration** in `saffron-control` (`commands_render.rs`, beside `set-displacement` at
  `commands_render.rs:900`): `reg.register::<SetTessellationQualityParams, SetTessellationQualityResult>(
  "set-tessellation-quality", …, |ctx, params| { ctx.renderer.set_tessellation_budget(edge_len,
  factor_cap, micro_tri_budget); Ok(…) })`. It sets renderer fields that feed the Phase-3 edge-factor +
  predict/prefix-sum kernels (their push/uniform budget inputs).
- **DTOs + manifest** in `saffron-protocol`: add `SetTessellationQualityParams` / `…Result` to `dto.rs`,
  the command entry to the `command.rs` manifest (mirror the `set-displacement` entry at
  `command.rs:230`), and surface the current budget on `RenderStatsDto` (`dto.rs:371`) alongside
  `skinning`/`displacement` — three read-back fields (`tessEdgeLength`, `tessFactorCap`,
  `tessMicroTriBudget`) so a test can prove the set took, not just that the call returned `ok`.
  Regenerate with `cargo run -p xtask -- gen-protocol` (and `bun run check` for `@saffron/protocol`).
- **e2e** (`tests/e2e/tessellation-budget.test.ts`, modelled on `toggles.test.ts`): boot a headless host
  with a displaced cube, `engine.call("set-tessellation-quality", { args: [<edgeLen>, <cap>, <budget>] })`,
  read `render-stats` back and assert the three fields changed (the read-back oracle the toggles suite
  uses — "ok:true alone proves nothing"), and assert `engine.validationErrors()` is empty across a few
  rendered frames (budget changes re-drive the tessellation passes, so this is the validation oracle).

### 5. Optional `VK_KHR_opacity_micromap` companion (fix #22 — non-gating)

Strictly optional, **no acceptance criteria, must not gate `COMPLETED`**; add only if/when alpha-cut
displaced materials exist. If added: probe `VK_KHR_opacity_micromap` in `device.rs` (mirror the
mesh-shader / Phase-1 probes; cross-vendor as of Vulkan 1.4.351), and attach an opacity micromap to the
**baked** triangle `TessellatedBlas` geometry (Phase 7) to cut any-hit cost on alpha-cut displaced detail.
It never touches the prism preview and is behind the `displaced-geometry → BLAS` seam.

### 6. Docs (keep-current rule)

- **Rewrite the one concept page** `docs/content/explanations/frame-and-render-graph/compute-displacement.md`
  to describe the **one mechanism** this planset builds (compute adaptive tessellation → transient VB/IB →
  shared by the seven raster passes via indirect draw **and** the RT BLAS), the `displaced-geometry →
  BLAS` **seam** (portable indirect/worst-case-pad floor; NV CLAS fast path; the approximate editor prism
  satellite), and the **approximate-preview** satellite (approximate on drag, exact on commit). Keep the
  existing `What | File | Symbols` table (symbols, not line numbers) — retarget its rows to the new
  symbols (`Tessellation` subsystem, the edge-factor/predict/dice kernels, `TessellatedBlas` vs
  `SkinnedBlas`, the prism march) and drop the "What this does not do yet / tracked in
  `plans/displacement/`" paragraph, since that folder is deleted here. Per NO-LEGACY there is **one**
  displacement page; do not add a second — this page *is* the mechanism's page. Retitle if the page name
  should read as the concept rather than the old pre-pass (e.g. keep `Compute displacement`, or `Adaptive
  displacement`), keeping the front-matter `title` equal to the body `# H1`.
- **Update the hub row** in `docs/content/explanations/frame-and-render-graph/_index.md` (the
  `Compute displacement` row, currently line 28) so its `Covers` + `Code` reflect the tessellating
  mechanism, the indirect raster consumption, and the RT seam.
- Voice: plain and direct; lead with the concept and why (the raster/RT single-source-of-truth), not
  "file X does Y"; run the prose through the `humanizer` pass.

### 7. Retire `plans/displacement/` and mark COMPLETED

Only after everything above lands and the gate is green:

- **Delete `plans/displacement/`** in its entirety (README + all `phase-*.md`). The AGENTS.md rule allows
  deleting a plan only once its superseding work is done; this planset is that work, and the old folder is
  now fully superseded (its B1/B3/C1/C2/D infrastructure is reused; its deferred hard problems are paid
  down across Phases 1–8). Update any cross-references that pointed at `plans/displacement/` (e.g.
  `plans/pending-ideas`, the docs page's removed paragraph, and this planset's README "Relationship"
  section, which can stop saying "the old phase files stay until the cutover lands").
- **Mark this planset `COMPLETED`**: set the `**Status:**` line in
  [`README.md`](README.md) to `COMPLETED` and flip each `phase-*.md` `**Status:**` line to `COMPLETED`
  (the OMM companion staying optional does **not** hold this back).

## Scope

- `saffron-rendering`: `rt.rs` (add `aabb_geometry` + the prism BLAS build/refit + a preview TLAS
  instance), a `prism_displace.slang` include + the shared height-sample include factored out of
  `displace.slang`, the inline prism march in the preview lighting path, and a renderer preview-mode flag +
  `set_tessellation_budget` (`renderer.rs`).
- `saffron-protocol` + `saffron-control`: the `set-tessellation-quality` command, its DTOs, the manifest
  entry, and the `RenderStatsDto` read-back fields; `xtask gen-protocol`.
- `editor`: engage/disengage the prism preview mode on `ViewId::AssetPreview` across a live displacement
  drag (begin-drag → prism, commit → baked swap), on the shipped material-graph live-preview host path.
- `tests/e2e`: `tessellation-budget.test.ts`.
- `docs/`: rewrite `frame-and-render-graph/compute-displacement.md`; update its hub `_index.md` row.
- `plans/`: delete `plans/displacement/`; mark this planset `COMPLETED`.

## Depends on

- **Phase 2** — the per-height-texture **min-max pyramid** (the march's acceleration structure) and the
  per-welded-vertex direction + UV-seam value/sampling-mode data (watertight prism side walls + equal
  seam height).
- **Phase 7** — the baked diced `TessellatedBlas` the preview is **exact against on commit**, and the
  `displaced-geometry → BLAS` seam the prism satellite sits beside (and the OMM companion attaches to).
- **Phase 1** — the keyed `acquire_buffer` (second transient consumer, no cursor desync) and the
  capability-probe scaffold (reused for the optional OMM probe). **Phase 3** — the budget knobs the
  control command feeds.

## Verification

- **Build/lint:** `just engine` then `just prepare-for-commit` (format + `cargo clippy -- -D warnings`)
  clean on the touched crates; `bun run check` green after `gen-protocol` (the wire change ripples to the
  typed client, like the D1 material-DTO change did).
- **CPU / control-plane (no GPU needed):** the new e2e `tessellation-budget.test.ts` — drive
  `set-tessellation-quality` over the control plane, read the three budget fields back through
  `render-stats`, and assert `engine.validationErrors()` stays empty across rendered frames. This is the
  language-appropriate gate for the control surface and runs in `just e2e`.
- **GPU-with-eyes (the crack-testing this engine needs):** on the available NVIDIA card, open the
  material-graph live preview on a displacement material and **drag the amplitude**:
  - The ray-traced shadow/reflection of the sphere updates **live with no BLAS rebuild** (no per-edit
    build stall) — the prism satellite is doing its job.
  - The lit surface has **no faceting** and **no seam light-leaks** at UV seams / prism boundaries
    (the `N' = Ns − Ng + N_shading` correction + Phase-2 seam agreement).
  - On **commit** (pointer-up), the view swaps to the baked diced `TessellatedBlas`; the silhouette may
    *shift slightly* (approximate → exact) but must not crack, and thereafter matches the raster silhouette
    (the Phase-7 check). Confirm the swap actually happens (the prism mode disengages — a rebuild fires
    once, not per-frame).
  - A Vulkan-validation-clean log throughout (the debug messenger prints none), captured as in the headless
    harness before any `pkill`.
- **Docs:** `cd docs && hugo` builds clean (SCSS compiles, no broken intra-site links after the row
  rewrite and the removed `plans/displacement/` reference).
- **Cross-vendor is deferred, not claimed:** the prism path is pure `VK_KHR_ray_query` + core AABB
  geometry, so it is architected portable, but AMD/Intel parity is not provable in the toolbox (no
  hardware GPU; limited/slow lavapipe RT) — state that, do not claim it proven.

## Risks

- **A second RT representation is a correctness foot-gun.** The prism march is approximate by construction;
  if it ever leaks into a shipping scene or gets described as `preview == scene`, the whole planset's
  headline guarantee is undermined. Wall it off hard: editor-only, `ViewId::AssetPreview`-only, live-drag
  only, torn down on commit, unreachable from `saffron-player`. Drop every "bit-identical / agrees exactly"
  phrasing in code comments and docs.
- **Inline AABB-candidate ray query is slower per-ray and hurts traversal coherence** than hardware
  triangle intersection. That is acceptable for a single-subject editor preview but would not be for a
  scene; keep prisms tight (Phase-2 `[min,max]`) and early-out aggressively, and never promote it.
- **AABB BLAS is new build surface.** `rt.rs` has only ever built triangle geometry; an AABB geometry with
  the wrong opaque flag or a stale `[min,max]` bound produces missed hits or over-march. Unit-test the
  prism-bound derivation (min/max from the pyramid × `height_scale`, three welded directions) on the CPU
  before trusting the GPU march.
- **Shared-sampling drift.** If the prism march and `displace.slang` ever sample the height differently
  (tiling, `height_scale` convention, filtering), preview and baked path diverge *in-the-small* on top of
  the intended silhouette approximation. Enforce one shared Slang include as the single height function;
  the Phase-4 world-space amplitude convention must already be pinned.
- **Transient cursor desync.** As the second `TransientResources` consumer the prism must acquire in fixed
  order every frame (zero-size when idle) or it desyncs the Phase-3 tessellator's slots. This is precisely
  the Phase-1 keyed-acquire contract — use it, do not add bespoke per-frame fields.
- **Deleting `plans/displacement/` is irreversible in-tree.** Do it only after the gate is green and every
  cross-reference is repointed; a dangling link to a deleted plan is a docs/link-check failure.
- **The OMM companion must not creep into the gate.** It is optional and non-gating; if it is not built,
  `COMPLETED` still holds. Do not let its absence (or an alpha-cut-material prerequisite that does not yet
  exist) block marking the planset done.
