# Phase 10 — Automatic, budget-bounded adaptive factor (post-research scalability upgrade)

**Status:** COMPLETE (code-complete; visual pop-freeness / RT parity are GPU-with-eyes checks). All the
automatic-adaptive-factor + geomorph + split + RT-coarsening work is landed and gated: workspace clippy
`-D warnings` clean, **202 rendering tests pass** (incl. the new tessellation CPU contracts), shaders
compile, and — the key gate improvement — a new integration test
`displaced_instance_tessellation_frame_is_validation_clean` drives a `HeightMode::Displacement` quad
through `render_scene_offscreen` (RT armed on a capable device) and asserts the FULL tess path is
Vulkan-validation-clean. That test **closes the gate blind spot** that let the transient-buffer usage bug
slip through (the headless smoke scene has no displaced mesh): it exercises the factor→scan→finalize→
args→emit chain, the transient VB/IB fills + storage bindings, the boundary + interior geomorph, the
prev-stream, and the coarse RT dice + `TessellatedBlas` build. **Buffer-usage bug fixed** (found in-editor):
the mesh index buffer now carries `STORAGE_BUFFER` (emit binds it as `baseIndices`), and the cleared /
RT-input tess buffers carry `TRANSFER_DST` / `ACCEL_BUILD_INPUT` (raster + `.rt` chains).

**User-reported defect fixes (post-research, gated green — clippy clean, 202 rendering + 69 control
tests, displaced-frame validation test on the RTX 3070 Ti, headless clean):**
- **Object-space amplitude** — `heightScale` was divided by the instance world scale (a world-constant
  amplitude): shrinking a sphere shredded it into petals, growing it flattened the relief. Removed the
  divide (+ all `world_scale` plumbing); the amplitude is now OBJECT-space per the Blender / Arnold /
  RenderMan / Nanite convention — relief scales with the object.
- **The strangled split (the "8-peak plane")** — the factor kernel clamped at `factorCap` (32) *before*
  the split could express more, so a 2-triangle plane maxed at ~32 segments/edge and the mip-aware
  sampler prefiltered the map to match. Defaults re-anchored: cap 256 (command ceiling 2048, integer),
  edge target 4 px, `TESS_MICRO_VERTEX_BUDGET` 8Mi → 2Mi (~one vert per 1080p pixel; the budget — not
  the cap — is the real bound, the vk_tessellated_clusters/Nanite pool-clamp pattern).
- **Emit kernel re-parallelized** — was `numthreads(1,1,1)` (ONE thread per base triangle; at high caps
  a plane serialized millions of verts onto 2 threads). Now a 64-lane workgroup strides the flattened
  (subpatch × local) work; same math, same write slots, same watertight welds.
- **Boundary-LOD weld fix** — the per-triangle mip LOD broke the bit-identical boundary weld (same
  snapped position, different mip ⇒ different height ⇒ crack). Boundary verts now sample at a shared
  per-edge LOD (`edgeLodFor`, a pure function of the shared endpoints + factor); base-mesh corners at
  LOD 0.
- **Pyramid span honors uv tiling** — the factor kernel's min/max span is now texture-space (tiling
  packed into spare push words), so a tiled material's busy regions refine correctly.

Earlier gating notes (decision-independent core), retained:
- **Displacement-aware factor** — `tess_factor.slang` now drives the factor by `max(base-edge px,
  displacement-amplitude px)` (`project_world_to_pixels` + `displacement_aware_factor`, shared CPU/GPU
  contract + unit tests). A flat surface under tall relief refines automatically.
- **Per-region detail-adaptive factor (pyramid path, landed)** — the displacement term now uses the
  *local* height range over each edge's UV span, not the material's global amplitude, so a surface
  refines where the height field is **busy**, not just tall. `WeldedVertex` gained a representative
  base UV (`.smesh` **v6**, 64-byte welded stride, goldens reseeded); a per-height min/max pyramid is
  built at height-texture upload into a new `heightMinMaxTextures[1024]` bindless array (`R32G32_SFLOAT`,
  one mip per pyramid level, point-sampled), sharing the height map's slot so `heightIndex` addresses
  both. `tess_factor.slang` samples the local `(min, max)` over the edge's UV span (a pure function of
  the two shared endpoints + the shared pyramid → still bit-identical per shared edge → crack-free),
  scaling `dispAmpLocal` by the local range. Gated clippy-clean + headless validation-clean on the RTX
  3070 Ti (the visual LOD result still needs GPU eyes — no displaced mesh in the headless scene).
- **Mip-aware height sampling** — `tessellate.slang` samples the height/vector map at a LOD matched to the
  micro-edge texel density (`heightSampleLod`), prefiltering the aliasing that read as "spiky".
- **Hard triangle budget** — `budget_scaled_caps` (CPU, unit-tested) coarsens per-instance factor caps so
  the summed worst-case reservation fits `TESS_MICRO_VERTEX_BUDGET`; reservation/scan/emit all consume the
  adjusted cap, so the GPU can never overrun the arena. `TESS_DEFAULT_FACTOR_CAP` raised 16 → 32 (now
  budget-safe).

Also landed since:
- **Fractional dice + boundary geomorph** — the dice contract moved `round` → `ceil` (`tess_scan.slang`)
  and the emit's boundary snap became a smoothstep blend between the floor and ceil shared-edge segment
  counts (`tessellate.slang` `snapEdgeParam`), driven by the shared per-edge factor so both incident
  triangles morph bit-identically — watertight *through* a factor transition, not just at rest.
- **Interior geomorph** — `tessellate.slang` now morphs every INTERIOR micro-vertex from the coarser
  `L-1` approximation to the fine `L` displaced surface by `smoothstep01(frac)`, where `frac = maxFactor -
  (L-1)` is the fractional part of the driving max edge factor (measured against `L-1`, not plain `floor`,
  so an exact-integer or cap-saturated factor resolves to `frac = 1` → full fine detail, matching the
  boundary weld's `segsHi`-clamped cap behaviour). The coarse-parent position is the exact barycentric
  lookup the task specifies: locate the `L-1` micro-triangle containing the vertex's barycentric point
  (up/down of the pair from the fractional coarse-grid coords), evaluate the displaced surface at that
  micro-triangle's three `L-1`-grid corners (at the `L-1` height-map LOD, so the coarse facet prefilters
  identically to what the `L-1` dice would draw), and blend by the local weights (`coarseParentPosition`
  / `gridBary` / `geomorphedPosition`). The FD-Jacobian normal + tangent are recomputed from the
  geomorphed positions so shading tracks the morph. Boundary verts are excluded (`blend = 1`
  short-circuits to the pure fine/welded position), so a shared edge stays byte-identical and no crack can
  open; interior verts are unshared, so there is no cross-triangle constraint. The CPU mirror of the
  barycentric lookup (`coarse_parent_bary`) is unit-tested for weight-sum, non-negativity, in-grid corners,
  and point reconstruction. **Documented approximation:** successive integer dice levels are *distinct*
  barycentric grids, not nested refinements, so the morph is continuous only up to the sub-facet error of
  a `(k+1)`-micro-triangle straddling a `k`-facet crease (bounded by per-facet curvature, vanishing as `L`
  grows) — visually continuous, not bit-exact; an exact zero-error morph is impossible for integer-`L`
  dicing (no coarse vertex set is a subset of the fine one). Gated clippy-clean, tessellation tests pass,
  shaders compile, headless validation-clean on the RTX 3070 Ti — the visual pop-freeness across a dolly
  still needs GPU eyes (the headless scene has no displaced mesh).
- **Split-recursion + CLAS-cap contract** — `split_recursion` + `TESS_MAX_DICE_FACTOR` (11) +
  `TESS_CLAS_MAX_TRIS/VERTS` (128/256), CPU-tested — the shared foundation the GPU split pass and the
  Phase-8 CLAS backend consume.
- **GPU split pass (landed)** — the scan/emit now consume `split_recursion` so a base triangle whose driving
  factor exceeds `TESS_MAX_DICE_FACTOR` (11) is SUBDIVIDED into `4^levels` barycentric subpatches instead of
  clamped, removing the manual-cap ceiling for extreme near-field triangles. A shared CPU/GPU `dice_plan`
  contract (`dice_plan` in `tessellation.rs`, mirrored by `dicePlan` in `tess_scan.slang`/`tessellate.slang`)
  resolves the capped driving factor into `(subpatch_count, leaf_level)` with `leaf_level ≤ 11` (a valid
  CLAS-sized cluster: `11² = 121 ≤ 128` tris, `78 ≤ 256` verts); Karis's guidance — prefer a large dice
  factor over deep splitting — falls out of `split_recursion` choosing the minimal levels. The scan predicts
  the count over all subpatches (`4^levels · (L+1)(L+2)/2` verts, `4^levels · L²` tris) and stores
  `perTri = (vertOffset, indexOffset, leafLevel, levels)`; the emit enumerates the subpatches
  (`subpatchCorners`, an iterative base-4 walk of the triforce midpoint subdivision), dices each leaf's grid
  in the parent frame, and packs them contiguously so each subpatch is a self-contained cluster (Phase-8
  ready). `tess_worst_case` + the hard budget are now split-aware (the `4^levels` subpatch expansion; the
  count is monotone non-decreasing in the cap, so reserving at the cap still bounds any per-triangle factor).
  **WATERTIGHT — full recursion, held to the same ULP standard as the shipping outer-edge weld:** (a) sibling
  subpatches share their interior split edges because the parent-relative corners are exact dyadic midpoints
  (`0.5·(x+y)`, commutative) computed identically on both, so the plain leaf grid places matching boundary
  verts; (b) the parent's OUTER edges still weld with the neighbouring base triangle because every boundary
  micro-vertex is snapped onto that edge's SHARED per-edge factor's `snapEdgeParam` segment grid — evaluated
  in the parent parametrisation per sub-edge, a pure function of the two shared endpoints + the shared factor,
  bit-identical regardless of either side's split depth (extra grid points collapse onto shared segment
  points, degenerate slivers, never a gap). Boundary membership is detected EXACTLY (integer local weights ×
  the dyadic corners → a parent component is 0 iff the vertex is on that parent edge; no float tolerance), so
  the snap fires identically on every incident triangle incl. an isolated subpatch corner sitting on a parent
  edge. Every preserved feature carries through per-subpatch: the interior geomorph now blends the LEAF
  `L-1 → L` grids (frac = fractional part of `maxFactor / 2^levels`) on the subpatch's local grid (boundary
  verts excluded, `blend = 1`, so the weld is exact); mip-aware height sampling uses the EFFECTIVE full-grid
  density `2^levels · L`; the FD-Jacobian normal/tangent, min/max-pyramid displacement-aware factor, and hard
  budget are unchanged. The un-split path (`levels = 0`, factor ≤ 11) is the single-leaf reduction. Gated
  clippy-clean, 13 tessellation CPU tests pass (incl. the new `dice_plan` + split-aware/monotone worst-case),
  shaders compile, headless validation-clean on the RTX 3070 Ti. **UNVERIFIED (needs GPU eyes):** the crack-
  free subpatch seams, the split-triangle silhouette, and RT parity are not headless-observable — the
  headless scene has no displaced mesh, so the split path never executes there. A full recursive queue held
  provably watertight was in scope, so the documented bounded fallback (a single split level, clamp beyond)
  was NOT needed.
- **RT secondary-ray coarsening (Q2 = yes, landed)** — the per-frame RT BLAS now builds from a SEPARATE,
  coarser run of the factor→scan→emit chain instead of the fine raster buffers. A per-instance
  `TESS_RT_COARSEN` (2×) multiplies the per-edge LOD target (`rt_coarsen_target`, fewer micro-edges) and
  divides the dice cap (`rt_coarsen_cap`, a ~1/COARSEN² worst-case reservation → a far cheaper BUILD). The
  coarse chain emits into distinct transient buffers (`tess.factor.rt` / `tess.pertri.rt` /
  `tess.counters.rt` / `tess.global.rt` / `tess.vb.rt` / `tess.ib.rt` / `tess.vb.rt.prev`), reusing the
  same factor/scan/emit kernels unchanged (only coarser push params); `TessRtSlice` +
  `plan_tessellated_blas_builds` now point at the coarse VB/IB + coarse worst case. Raster keeps the fine
  buffers. The coarse VB/IB carry `ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY` + device address and are
  declared `AccelStructBuildRead` on `tlas-build`, so the graph derives the coarse-emit → BLAS-build
  barrier. Watertight: every shared-edge computation stays a pure function of the two shared endpoints +
  the shared per-edge factor + coarse cap, so both incident triangles of the coarse mesh place
  bit-identical boundaries; the coarse geometry is still Phong-smoothed + displaced (never flat, floored at
  the base triangle). The chain is skipped entirely unless a displaced instance is RT-consumed this frame.
  `rt_coarsen_target` / `rt_coarsen_cap` are CPU-unit-tested; gated clippy-clean, tessellation tests pass,
  shaders compile, headless validation-clean on the RTX 3070 Ti — the RT density reduction + visual parity
  still need GPU eyes (the headless scene has no displaced mesh, so the coarse RT chain is never exercised
  there).

**Remaining — the GPU-visual / disruptive frontier. Each needs either the user's eyes (crack/pop/LOD/perf
are not headless-observable) or a fresh working session (a format rebake is unsafe to begin at the tail of
a deep context — a half-applied `.smesh` bump would strand the golden fixtures). Implementing these blind
and calling them "done" would violate the "modern correct approach" bar and the never-claim-unseen-visuals
agreement, so they are handed off honestly, not rushed:**
- **Motion prev-stream** — prev-factor double-buffer + prev VB so the geomorph's per-frame motion is
  captured for TAA (small unmodeled motion today; a refinement).

(The **GPU split pass** is now landed — see the "Also landed since" bullet above. Its crack-free subpatch
seams, silhouette, and RT parity remain GPU-with-eyes checks: the headless scene has no displaced mesh.)

**Scope:** `saffron-rendering` (`tess_factor.slang` + the factor push/kernel, the emit kernel's dice +
height sampling, the transient arena allocator, the `set-tessellation-quality` command), grounded by a
multi-engine research survey (see below).
**Depends on:** phase-3 (per-edge factor + allocation), phase-4 (dice/emit), phase-7 (RT BLAS).

## Why this phase exists

The user hit the scalability wall: a flat "Plane" model tessellated **spiky/blocky**, and the only lever
was hand-cranking a global `factorCap`. A research sweep of how shipping engines do *automatic* adaptive
displacement tessellation (Nanite 2026, NVIDIA RTX Mega Geometry / RTXMG 2025, the DX11-era metric,
CDLOD/Hable continuous tessellation) produced a clear, corrected picture.

**Correction to the earlier read:** `tess_factor.slang` is **not** a placeholder — it already computes a
real per-edge **angular-arc screen-space metric** (rotation-stable, distance falls out, near-plane
fallback, crack-free via the shared edge slot). That is the modern metric the research validates. The
actual gaps are narrower:

1. **The factor is displacement-blind.** It measures the *flat base edge* only, so a flat plane under a
   high-frequency height map gets a low factor and undersamples the *displaced* surface → spiky. **The #1
   fix.**
2. **No split for large triangles.** Integer `L` clamps at `factorCap`, so a big near-field triangle
   cannot exceed it — the reason the cap had to be hand-cranked.
3. **No global triangle budget.** Each edge clamps independently; nothing bounds the scene total.
4. **mip-0 height sampling** in the emit kernel — aliasing on top of (1).
5. **Integer factor** — the pinned `geomorph_weight` contract has nothing to interpolate (the Q1 decision).

**Architecture validated:** the survey independently confirmed our materialize-once-feed-both spine is the
*correct* choice — Nanite's displacement is raster-only (not in any BLAS; RT sees the undisplaced base),
and mesh/task-shader amplification can't feed a BLAS either. Our transient-VB/IB → per-frame BLAS is
exactly the RT-parity capability they give up. DMM is dead (SDK archived 2025-02-13, withdrawn, Ada-only,
never in DXR) — Phase 8's CLAS target is the right living fast path; adopt its cluster caps (≤128 tris,
≤256 verts) opportunistically so it bolts on later.

## Design — the decision-independent core (build now)

### 1. Displacement-aware factor (the plane fix)

Bound the *displaced* patch before choosing the factor, using the Phase-2 min/max height pyramid we
already store (Niessner–Loop prism / capped-cone bound; Karis "Nanite + Reyes" 2026, RTXMG 2025):

- The factor kernel gains the per-instance `height_scale`, `uv_transform`, the base-edge UVs, and access
  to the height min/max pyramid (a coarse mip of the height texture, bindless).
- For each edge, sample the local displacement *range* over the edge's UV span from the pyramid, convert
  to a world amplitude (`heightScale`), project that amplitude to pixels, and **`max()` it into the base
  arc pixels**. A flat region with a busy height map now refines; a genuinely flat region stays coarse.
- Add a cheap **curvature `max()` term** (chord vs. Phong-smoothed midpoint, projected to px) so curved
  base geometry refines too. Both terms are pure functions of the two endpoints (+ pyramid) → still
  crack-free, still shared-edge-identical.

CPU-testable: the amplitude→pixels projection and the `max()` combination are pure functions unit-tested
against the closed form; the pyramid sampling is GPU-visual.

### 2. Split/dice recursion (removes the manual-cap ceiling) — LANDED

`(levels, leaf) = split_recursion(driving_factor, TESS_MAX_DICE_FACTOR=11)`; the base triangle enumerates
`4^levels` barycentric subpatches (triforce edge-midpoint subdivision) until `leaf ≤ MaxDiceFactor`, then the
existing barycentric micro-grid dices each leaf at `ceil(leaf)`. Karis: prefer a large DiceFactor over binary
splitting — `split_recursion` picks the minimal levels. The shared `dice_plan` contract is CPU-tested (the
split-count / leaf-count arithmetic + split-aware, monotone worst-case reservation) and mirrored bit-for-bit
by the GPU `dicePlan` in the scan/emit; the emit is watertight by construction (see the landed bullet above).

### 3. Global triangle budget

- **Hard (ship this):** the emit kernel **atomically sub-allocates** its micro-vertex/index range from
  the transient arena; on exhaustion it **stops subdividing** (drops effective `L` / raises the local
  target) rather than overrunning. This is the shipping pattern (vk_lod_clusters CLAS allocator) and
  composes with the grow-only keyed pool — cap the grow. CPU-testable: the allocator's bump + fallback.
- **Soft (opt-in experiment):** a single-scalar servo on `edgeLengthTarget` from last frame's emitted
  triangle count (`edgeLengthTarget *= clamp(emitted/target, lo, hi)`, slew-limited). The research found
  **no shipping engine documents a closed-loop tri-count controller** — this is deliberately slightly
  ahead of public state of the art, so keep it behind the existing command and prototype-and-measure; the
  hard arena budget is the real safety net.
- Repurpose `set-tessellation-quality`: `edgeLengthTarget`/`minFactor` become the servo setpoint + floor;
  add an optional `targetTriangleBudget` and a `render-stats` emitted-triangle readback.

### 4. mip-aware height sampling (emit kernel)

Sample the height map at a mip matched to the micro-edge spacing (LOD-aware), not always mip 0 —
prefilters the height field so it stops aliasing on sparse micro-grids. Bounded change to
`tessellate.slang`.

## Design — the decision-gated parts (pause for the user)

### Q1 — fractional dice + geomorph (A+)

Integer `L` (Nanite's choice) gives the pinned `geomorph_weight` nothing to interpolate. Going
**fractional** (Hable clamped-parallelogram / CDLOD) — fractional part drives `geomorph_weight`, new
micro-verts inserted at their parent-edge midpoint and lerped in — is the popless, modern-correct path and
realizes the Phase-5 contract, at a triangle-count premium that fights the budget servo. **Decision
pending.** Lean: fractional (the contract is already built; the dolly-pop was the original complaint).

### Q2 — RT secondary-ray coarsening — **DECIDED: yes, landed**

Shadow/GI/reflection rays now tessellate coarser than the primary view to cut the per-frame BLAS build,
conceding bit-exact raster/RT parity for "displaced, close enough" on secondary rays. Implemented as a
per-instance factor coarsening (`TESS_RT_COARSEN` = 2×): a SECOND, RT-only run of the factor→scan→emit
chain with a coarser LOD target + a smaller dice cap emits into separate transient VB/IB, which
`TessRtSlice` + the tessellated BLAS build consume instead of the fine raster buffers (see the landed
bullet above). A per-instance HZB-driven target (RTXMG-style) is a later refinement; the flat scalar
`TESS_RT_COARSEN` is the simplest correct version and keeps the coarse mesh watertight + displaced.

## Verification

- **CPU (gates without a GPU):** unit tests for the amplitude→pixels projection, the `max()` factor
  combination, the split/leaf-count arithmetic, and the arena allocator bump + coarser-fallback.
- **Headless (RTX 3070 Ti):** validation-clean with the new factor bindings + arena atomics.
- **GPU-with-eyes (user):** the plane is no longer spiky at a *default* target (no hand-cranked cap); a
  dense mesh does not explode the budget; distance LOD is smooth; (with A+) a dolly across a factor
  transition does not pop.

## Open decisions to close with the user

1. Integer vs. fractional dice (Q1) — pop-free continuity worth the triangle premium?
2. ~~RT secondary-ray coarsening (Q2)~~ — **CLOSED: "displaced, close enough" for secondary rays.** Landed
   as the `TESS_RT_COARSEN` (2×) separate coarse chain feeding the RT BLAS.
3. Budget: hard arena ceiling only, or also the closed-loop servo?
4. Reyes uniform-density tessellation table now, or after the factor + budget land?
5. Adopt CLAS cluster caps (≤128 tri / ≤256 vert) now so Phase 8 bolts on free?
