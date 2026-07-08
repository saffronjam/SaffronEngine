# Phase 5 — Temporal correctness — identity-free motion vectors + geomorph continuity

**Status:** IN PROGRESS — the CPU-gate-able contract and the static-displacement motion floor are landed
and gated (workspace clippy `-D warnings` clean, tessellation tests pass, headless validation-clean):
- **`geomorph_weight(remainder, levels_since_birth)` + `smoothstep01`** in `tessellation.rs` — the shared
  numeric contract for §3/§4, unit-tested for the birth→resolve ramp (0→1), full resolution of older
  vertices, monotonicity, and **C0 across an integer factor boundary**. This is the one definition the
  emit kernel and the CPU test must agree on bit-for-bit.
- **The motion pass consumes the tessellated stream** (`aa.rs:365`): a tessellated batch binds its
  amplified transient VB as *both* the current and previous position stream (+ the generated index
  buffer), so `motion.slang` emits pure object+camera motion of each mesh-local surface point. For a
  **static** height field the surface is fixed in mesh-local space, so `prev == cur` is exact — this is
  the correct floor (identical to the retired displace prev-set semantics), not an approximation.

**The geomorph blend (§3) is landed** on the shared cur stream for BOTH boundary and interior
micro-vertices (`tessellate.slang`): boundary verts blend floor→ceil shared-edge segment placements
(`snapEdgeParam`, watertight by the shared per-edge factor); interior verts morph from the coarser `L-1`
approximation to the fine `L` displaced surface by `smoothstep01(frac)` via the exact coarse-parent
barycentric lookup (`coarseParentPosition`; the FD normal is recomputed from the geomorphed positions).
Both write the single shared VB the raster passes and the RT BLAS read, so raster and ray-traced
silhouettes stay bit-identical through a factor transition. (Documented limitation: interior continuity is
sub-facet-exact, not bit-exact — successive integer dice levels are distinct grids, not nested
refinements; see Phase 10.)

**Remaining — GPU-with-eyes refinements (need a presenting GPU to verify no-smear):** (A) the
**identity-free reconstruction** — a separate prev VB written by the emit kernel from the *previous*
frame's inputs (double-buffered per-edge factors + per-instance height/uv/transform), needed only when the
height field **animates**; (B) **capturing the geomorph's per-frame motion for TAA** — re-run the same
blend against the *previous* frame's `remainder` into that prev VB so a dolly across a factor transition
reads as a small, continuous cur/prev delta rather than being unmodeled (today the geomorph moves the
surface on the cur stream but the prev stream still binds the cur VB, so its motion is not yet captured).

**Scope:** `saffron-rendering` (the motion-vector pass in `aa.rs`, the Phase-4 emit kernel, the
double-buffered per-instance / per-edge factor records, the Phase-1 keyed transient acquire)
**Depends on:** phase-4-dice-displace-weld-emit.md

## Goal

Make a re-diced displaced surface temporally stable under a moving camera: correct per-pixel motion
vectors with **no per-vertex history buffer**, and smooth geomorph across integer-factor transitions so
a dolly/zoom never pops. This phase is the dedicated owner of the two temporal fixes the architecture
calls out — **#9 (identity-free previous position)** as the motion-pass *consumer* of the Phase-4
producer, and **#10 (geomorph)** as the source of the blend that keeps a factor transition from
becoming a motion-vector spike.

The hard fact this phase is built around: the tessellator re-dices every frame, so the micro-vertex
count and identity change frame-to-frame, and the previous frame's transient vertex/index buffers are
already overwritten (the transient pool is grow-only and rewound each frame — `transient.rs`
`begin_frame`). There is therefore **no vertex→vertex correspondence** to sample a history buffer with.
The only correct previous position is one *reconstructed* from the same barycentric sample against the
previous transform + previous height, which Phase 4 emits into a parallel stream. This phase wires that
stream through the existing two-stream motion pass and adds the geomorph blend that makes the delta it
carries small and continuous.

## Background — how motion vectors already work (the seam this phase extends)

The motion prepass is already a **two-position-stream** pass, which is exactly the shape this phase
needs — it was built for skinning/morph deform-motion and generalizes to the tessellated streams with no
new pass:

- `assets/shaders/motion.slang` `vertexMain` reads two vertex streams: binding 0 `position` (this
  frame's deformed position) and binding 3 `prevPosition` (the previous frame's deformed position),
  and emits `curClip = curViewProj·model·position`, `prevClip = prevViewProj·prevModel·prevPosition`.
  `fragmentMain` outputs `(prevNdc − curNdc)·0.5` into the RG16F motion image that TAA reads.
- `aa.rs` `record_motion` binds those two streams per batch at **`aa.rs:370-371`**
  (`cmd_bind_vertex_buffers(cmd, 0, &[cur, prev], …)` then `cmd_bind_index_buffer(…, batch.mesh.index_buffer(), …)`),
  choosing them via `select_motion_streams(batch.deformed, deformed, prev_deformed, batch.mesh.vertex_buffer())`:
  a deforming batch with both `Skinning` deformed buffers present reads `(deformed, prev_deformed)`;
  every other batch binds the same static stream to both, so `prevPosition == position` and motion is
  pure object motion from `inst.prevModel`.
- `MotionPush` carries `curViewProj` + `prevViewProj`; `inst.model`/`inst.prevModel` come from the
  set-2 `InstanceData` SSBO (which already double-buffers `prev_model` — object motion is handled and
  unchanged here).

The current `displace` subsystem already exercises this correctly for the 1:1 case: `Displacement::wire_dispatches`
allocates a **cur** set (base → `deformed`) and a **prev** set (base → `prev_deformed`) and, because the
displacement is static, both write byte-identical vertices, so the deform delta is zero and the motion
pass sees pure object motion. Phase 5 keeps that exact model — cur stream and a parallel prev stream at
the same slots — but for the *tessellated* buffers, where the prev stream can no longer be "write the
same thing again" (the sample set changed) and must instead be the identity-free reconstruction.

## Build plan

### 1. Consume the Phase-4 identity-free previous-position stream in the motion pass

Phase 4's emit kernel writes, per emitted micro-vertex **slot `i`**, two positions into two transient
buffers acquired at two stable Phase-1 keys (`"tess.vb"`, `"tess.vb.prev"`):

- the **current** mesh-local displaced position → the current tessellated VB (the Phase-4 output every
  raster pass and the BLAS read), and
- the **previous** mesh-local displaced position of *the same barycentric sample* — re-evaluated
  against the previous transform state + previous height/factors (fix #9 producer) → the prev
  tessellated VB.

The correspondence that makes this work is **within-frame by slot**, not across frames: the generated
index buffer (Phase 3/4) references slot `i`; binding the current VB at binding 0 and the prev VB at
binding 1 makes indexed draw pull `cur = curVB[i]` and `prev = prevVB[i]` for the *same* micro-vertex,
and `prevVB[i]` already holds that sample's previous mesh-local position. No stale buffer is read; the
index buffer and both position streams are all this-frame data.

Extend the motion-pass binding in `aa.rs`:

- Add a tessellated arm to `select_motion_streams` (or a sibling selector): a **tessellated** batch
  (the Phase-6 `DrawBatch` flag / generated-index handle) binds `(tess_vb, tess_vb_prev)` at bindings
  0/1 and the **generated** index buffer (not `batch.mesh.index_buffer()`) at `aa.rs:371`. Keep the
  existing three cases (skinned/morph deformed → `(deformed, prev_deformed)`; static → `(static, static)`)
  unchanged.
- The draw itself becomes indirect over the Phase-3 args/count buffers; that conversion is owned by
  **Phase 6** (which makes all seven passes indirect via `record_batch_submeshes`). This phase supplies
  the *prev-stream binding + semantics*; Phase 6 supplies the *indirect draw*. Because the motion pass
  is one of the seven consumers of the single tessellated buffer written in the deform scope, it reads
  the same geometry depth/shade/shadows read — a per-pass re-dice is impossible, so cur/prev can never
  desync between passes.
- `motion.slang` needs **no shader change**: it already consumes two position streams and both clip
  matrices. `model`/`prevModel` stay the node world / previous-node-world matrices from `InstanceData`
  (displaced micro-vertices are mesh-local, matching the `DeformedRtInstance` `world_transform` = node
  model convention), so object motion and deform motion compose exactly as they do for skinning.

### 2. Retain the reconstruction inputs — double-buffer the small per-instance / per-edge records

The identity-free reconstruction (executed in the Phase-4 emit kernel for the prev slot) needs the
*previous frame's* inputs for each base edge and each instance. These are small and must survive one
frame:

- **Per-edge fractional factors** (Phase 3, one factor + fractional remainder per unique base edge):
  double-buffer the per-edge factor buffer into a two-slot `{cur, prev}` ring, mirroring how
  `Skinning` owns `deformed_buffer` / `prev_deformed_buffer`. Base topology (and hence the unique-edge
  set) is fixed, so "the previous factor of edge `e`" is always well-defined even though the emitted
  sample set changed — this is what lets a this-frame barycentric sample be re-evaluated at last
  frame's subdivision state.
- **Per-instance displacement state** — previous `height_scale`, previous `uv_transform`, previous
  factor-bucket (Phase 3 LOD bucket), and the previous transform. The transform is already covered:
  `InstanceData.prev_model` double-buffers the node world matrix. Add the remaining scalars to a
  compact double-buffered per-instance record (a two-slot ring, same acquire discipline). For a static
  height field these previous values equal the current ones, the reconstructed prev mesh-local position
  equals the current one, and the motion collapses to pure object motion — the correct, already-proven
  zero-deform-delta behaviour of the current `displace` prev-set.

Acquire the prev tessellated VB and the prev factor/record buffers through the **Phase-1 keyed
`acquire_buffer(frame, key, bytes, usage)`** at fixed keys every frame (zero-size acquire on a frame
with no tessellated instances) so the cursor never desyncs against the other transient consumers
(fix #16). Size the prev VB to the **same worst-case bound** as the current VB (Phase 3) so slot `i`
is valid in both.

### 3. Geomorph across integer-factor transitions (fix #10) — the blend that prevents the spike

Without geomorph, an integer factor increment (a row of micro-vertices appearing as the camera dollies
in) is a discrete topology change: the newly born vertices snap from nonexistence to full displacement
in one frame, and the reconstructed prev position of the surface at those pixels jumps → a large
single-frame motion-vector discontinuity, which TAA reads as smear/ghost at exactly the high-frequency
displaced silhouettes.

The fix is to make the transition a **smooth motion** instead of a discrete one, so a re-dice looks like
any other continuous surface movement and the motion vector it produces is small and correct:

- Drive the blend from the **Phase-3 fractional remainder** already stored per edge. A micro-vertex
  born at subdivision boundary carries a geomorph weight `w = geomorph_weight(remainder, birth_level)`:
  as the fractional factor crosses the integer boundary (`remainder` sweeping `0→1`), a newly split
  ("odd") vertex morphs from its **coarse-parent position** (the linear interpolation of its two even
  neighbours along the parent edge — the position it would occupy at the lower integer factor) to its
  **fine diced+displaced position**. Interior samples derive their weight from the three edge factors,
  as the base position already does (Phase 4).
- Apply the blend **inside the Phase-4 emit kernel**, to the mesh-local position that is written to the
  *shared* current tessellated VB — so raster and the RT BLAS (Phase 7) both read the geomorphed
  surface and stay bit-identical (a geomorph applied only for raster would re-introduce the RT/raster
  silhouette divergence the whole spine exists to kill). The prev slot re-runs the *same* blend with
  the *previous* frame's `remainder` (from the double-buffered per-edge factors), so a geomorph shows
  up as a smooth cur/prev delta, not a pop.
- **Watertightness is preserved for free:** Phase 3 guarantees both incident triangles of a shared edge
  compute a bit-identical factor *and remainder* from the two endpoints only, and Phase 4 welds the
  boundary micro-vertices onto the interpolated base edge. The geomorph is a function of that shared
  remainder evaluated at the shared boundary sample, so both sides blend identically — no crack opens
  during the transition, in either the raster silhouette or the ray-traced one. Apply the identical
  weld to the prev stream too, or the *previous* silhouette cracks and motion vectors leak light at
  seams.

### 4. Factor the remainder→weight mapping as a pure, CPU-testable function

Implement `geomorph_weight(remainder, birth_level) -> f32` (and any parent-sample index helper) as a
plain Rust function in the tessellation crate — the numeric contract shared by the CPU unit test and the
kernel (a small shared constant/algorithm, exactly as `skinning.rs`/`transient.rs` keep their sizing
math CPU-testable). Kernel and host must agree on it bit-for-bit conceptually so the test actually
covers the shipped behaviour.

## Watertightness / RT / normal-correctness notes

- **RT stays exact.** Motion vectors never feed the BLAS, but the geomorph *does* change the mesh-local
  position, so it must live in the single shared emit (§3) that both `scene_pass` (raster, Phase 6) and
  `rt.rs` (BLAS build, Phase 7) consume. Then the ray-traced silhouette equals the rasterized one
  during a factor transition, not just at rest.
- **Seam-consistent prev evaluation.** The prev stream re-evaluates the same height (via the Phase-2
  per-edge seam sampling mode) and the same welded direction/tangent as the cur stream, so the previous
  surface is as watertight as the current one; otherwise the reconstructed previous silhouette cracks
  and TAA sees phantom motion at UV seams.
- **Shadow/point-shadow passes are untouched by this phase.** Only the motion pass binds a prev stream;
  the depth/shadow/GBuffer passes bind one position stream and pick up the geomorphed positions
  automatically from the shared cur VB (their indirect-draw conversion is Phase 6).
- **Object motion is unchanged.** `InstanceData.prev_model` already carries the previous node world
  matrix; this phase only adds the *deform* (mesh-local) prev position, which composes with it in
  `motion.slang` exactly as the skinned path does.

## Verification

Logic-level (CPU, gates without a GPU):

- Unit-test `geomorph_weight`: assert it is `0` at a vertex's birth (remainder → the split boundary),
  `1` when fully resolved, monotone in `remainder`, and **C0 across the integer boundary** (the weight
  a just-born vertex reads at `remainder → 1⁻` of level `N` equals the weight the same sample reads at
  `remainder → 0⁺` of level `N+1`) — i.e. no discontinuity is smuggled into the mapping itself. Add a
  paired assertion that a shared edge's two incident triangles receive the same `(factor, remainder)`
  (re-using the Phase-3 shared-factor test) so the geomorph is provably seam-identical.
- A keyed-acquire ordering test (extends the Phase-1 test): with tessellated instances present one
  frame and absent the next, assert the prev VB / prev-factor slots stay at their keyed positions and
  the pool cursor does not desync.

Visual (needs a GPU — the crack-testing this genuinely requires):

- Run headless in the toolbox on the NVIDIA card (`just run-engine-headless [frames]`, which exports the
  correct `VK_ADD_DRIVER_FILES` nvidia_icd path; or `SAFFRON_EXIT_AFTER_FRAMES=N` with the
  `nvidia_icd` env) on a scene with a **dollying** camera over a high-frequency authored-Displacement
  surface (a displaced rock/terrain), TAA enabled.
- Capture the motion image via `motion_visualize.slang` across the frames where the camera crosses an
  integer-factor transition: assert **no discontinuity band** appears in the motion field at the
  transition (a discrete re-dice would show a bright seam of spurious velocity sweeping the silhouette;
  the geomorph must make it continuous).
- Capture the resolved TAA output over the same dolly: assert **no ghosting/smearing at displaced
  silhouettes** (the failure mode of a wrong prev position) and no shimmer at UV seams (the failure
  mode of an inconsistent prev weld).
- The whole run must be **validation-clean** (the gate's log assertion; barriers for the extra prev VB /
  factor rings are graph-derived, so a validation hit means a missing usage declaration, not a
  hand-barrier bug).

## Risks

- **A sample's barycentric domain can vanish across a large topology change.** Identity-free
  reconstruction assumes the same barycentric sample existed (or has a well-defined previous position)
  last frame; under a very large single-frame factor jump the reconstruction is approximate at those
  micro-vertices. Fractional factors + geomorph keep per-frame factor change small (a smooth sweep, not
  a jump), which is precisely what keeps the approximation tight — so geomorph is *required for
  correctness here*, not just for aesthetics. A hard factor clamp / hysteresis (Phase 3/7) bounds the
  worst case.
- **Prev-stream cost.** A second worst-case-sized tessellated VB and the double-buffered factor/record
  rings add VRAM (× `MAX_FRAMES_IN_FLIGHT`) and emit-kernel work (the prev slot re-evaluates the same
  sample). This compounds Phase 3's grow-only peak-hold pressure; it rides the Phase-3 shrink/reclaim
  path and the factor budget, and must be accounted in the tessellation VRAM budget rather than assumed
  free.
- **Kernel/host geomorph divergence.** If the shader's geomorph and the CPU `geomorph_weight` drift, the
  unit test passes while the surface still pops. Keep the mapping a single shared definition and treat
  any visual pop with a green unit test as a divergence bug, not a tuning problem.
- **Seam geomorph is a watertightness dependency, not a nicety.** The blend must use the shared per-edge
  remainder and the shared weld on *both* cur and prev streams; a mismatch cracks the transitioning
  silhouette (and, via Phase 7, the ray-traced one) and leaks light — the same failure class Phase 2/3/4
  guard, now extended across time.
- **Coupling with Phase 6's indirect conversion.** This phase defines the prev-stream binding but the
  motion pass only becomes fully indirect in Phase 6; until then the motion arm must draw over the
  generated index buffer with a CPU-known or count-driven draw, consistent with the other passes, so
  cur/prev never desync from depth/shade.
