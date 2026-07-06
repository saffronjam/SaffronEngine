# Phase 4 — dice + displace + weld + emit (the amplifying kernel) + retire the 1:1 path

**Status:** NOT STARTED

The load-bearing phase: the compute kernel that *amplifies* base triangles into new micro-vertices and a
generated index stream, and the NO-LEGACY cutover that deletes the 1:1 `displace.slang` kernel in the same
change. Phase 3 has already decided *how much* to emit (fractional per-edge factors) and *where* to write
it (predict→prefix-sum per-triangle offsets, `cmd_dispatch_indirect` args, worst-case-sized transient
VB/IB); Phase 2 has already produced the watertight conditioning (edge adjacency, per-welded-vertex
direction + seam-consistent tangent seed, UV-seam value-agreement flags, min-max pyramid). This phase is
the emit stage that consumes both and writes one geometry stream that Phase 6 rasterizes and Phase 7
builds the BLAS from.

## Goal

Replace the one-thread-per-vertex bijection in `assets/shaders/displace.slang::computeMain`
(`oBase = (push.deformedOffset + i) * VertexStride`, confirmed at `displace.slang:45`) with a
per-triangle/patch domain, driven by Phase 3's `cmd_dispatch_indirect`, that for each base triangle:

1. dices it with a clamped-parallelogram grid over **one** pinned edge-continuous smoothed base (Phong
   tessellation), with a specified matched-gap **stitch** triangulation and no T-junctions;
2. evaluates the smoothed-base position at each barycentric sample;
3. displaces scalar-along-normal **or** tangent-space vector (branch on `vectorIndex != 0`, reusing
   `DisplaceInfo.height_index` / `vector_index` and the bindless set-0 albedo array), honoring Phase 2's
   per-edge seam sampling mode so a seam edge reads one height value;
4. re-derives a **welded, seam-consistent tangent frame** (normal **and** tangent) per micro-vertex from
   the **full position Jacobian** — base curvature plus the displacement derivative, a full 3×3 for vector
   displacement — not a scalar height gradient;
5. **welds** shared-edge / UV-seam micro-vertices using Phase 2's per-welded-vertex direction, snapping
   boundary micro-vertices exactly onto the interpolated base edge and evaluating the same
   height+direction+tangent there;
6. writes the displaced micro-vertices **and** the generated index stream at the Phase-3 offsets into the
   transient VB/IB, and additionally emits each micro-vertex's **identity-free previous clip position** for
   motion vectors (Phase 5 consumes it);

reading the **skinned** deformed-ring output as its base for skinned+displaced instances (runs *after*
skinning inside the deform scope), and pinning **one** world-space `height_scale` amplitude convention
shared by preview and scene. Then delete the 1:1 kernel and its ring reservation.

## Build plan (concrete, grounded in the existing `Displacement` subsystem)

`displacement.rs` is the exact template — a new `saffron_rendering::Tessellation` subsystem mirrors its
shape (set-1 layout + per-frame descriptor pools + `wire_dispatches` / `record_*` / `request_*_pipeline`),
but the kernel domain, the buffers it binds, and the deform-scope pass change. The seam it slots into is
the `if do_displace` block at `renderer.rs:3862` (`RgPass::compute("displace")` at `renderer.rs:3870`).

### 1. The dispatch domain: per-triangle/patch, indirect

`assets/shaders/tessellate.slang::computeMain` (the replacement for `displace.slang`) is dispatched via
`cmd_dispatch_indirect` reading Phase 3's args buffer — the domain is one workgroup per base triangle (or
per output tile of a large triangle), **not** one thread per input vertex. Each invocation:

- reads its base triangle's three corner `Vertex`es (position@0, normal@12, uv0@24, tangent@32 — the same
  48 B layout `displace.slang` already loads at `displace.slang:11-14`) from the **base-in** binding;
- reads its three per-edge fractional factors + the per-triangle vertex/index **write offsets** produced
  by Phase 3 (`StorageReadCompute` on those buffers);
- derives inner factors from the three edge factors (Phase 3's rule), builds the clamped-parallelogram
  grid, and emits micro-vertices + indices at the offsets.

The base-in binding is per-bucket: for a pure-Displacement instance it is the mesh's **static** vertex
buffer (`GpuMesh::vertex_buffer()`); for a **skinned+displaced** instance it is the **skinned
deformed-ring slice** (see §7). This is set-1 binding 0, the exact slot `displace.slang` reads from today.

### 2. Dicing — clamped-parallelogram grid + Phong base + matched-gap stitch

Pin **exactly one** smoothed-base scheme: **Phong tessellation** (Boubekeur & Alexa) — cheap, needs only
the stored positions + normals, no extra control data. For barycentric `(u, v, w = 1−u−v)` the smoothed
position is the barycentric point projected onto each corner's tangent plane and blended:
`P(u,v) = Σ bᵢ · πᵢ(Σ bⱼ pⱼ)` where `πᵢ` projects onto vertex `i`'s tangent plane (from `pᵢ`, `nᵢ`).

**Edge-curve-from-endpoints invariant (watertightness link 1).** A shared edge's Phong curve is a function
of only its **two endpoints and their normals** — it never references the triangle interior or the third
vertex. Both incident triangles therefore evaluate a bit-identical boundary curve, so the *smoothed base
itself never cracks*, independent of the two triangles' differing interior grids. This is the DiagSplit
local-edge principle applied to the base surface.

**Dicing pattern.** Form a parallelogram from the two spanning edge factors and clamp to the triangle
(`u = min(u, 1−v)`), giving a regular quad interior with clean edge loops (Filmic Worlds "Compute
Tessellation with Clamped Parallelograms"). Point insertion is **sequential**, not DX11 even-spacing, so a
fractional factor grows the grid one row at a time (pop-free; Phase 5's geomorph rides the fractional
remainder Phase 3 stored).

**Matched-gap STITCH topology (no T-junctions).** When an edge's fractional factor is coarser than the
interior grid's row count, reconcile the two with an explicit **fan/staircase** strip along that edge: the
boundary carries exactly `⌈factor⌉` segments, the first interior row carries the grid's row count, and the
gap between them is triangulated as a fan (a single boundary vertex to a run of interior vertices) or a
staircase (alternating), chosen so every interior vertex and every boundary vertex is an endpoint of an
emitted triangle — no vertex lands on the interior of another triangle's edge. The stitch is table-driven
off the two counts (the same precomputed-LUT idea as the interior grid), so the reconciliation is
precision-exact and identical on both sides of the edge.

### 3. Displace — scalar-along-normal or tangent-space vector

Reuse the `displace.slang:24` branch verbatim in structure. Build the true UV-aligned TBN from the
smoothed-base tangent frame (`t = normalize(tangent.xyz − n·dot(n, tangent.xyz))`, `b = cross(n,t)·w`,
exactly `displace.slang:17-18`), then:

- **scalar** (`push.vectorIndex == 0`): sample `h` from `albedoTextures[height_index]` at the sample UV,
  `displaced = P + N·(h · heightScale_local)` (§8 for the amplitude convention);
- **vector** (`push.vectorIndex != 0`): sample the tangent-space XYZ from
  `albedoTextures[vector_index]` (the `*2 − 1` decode at `displace.slang:54`),
  `displaced = P + t·v.x + b·v.y + N·v.z` (overhangs), matching `displace.slang:29`.

`DisplaceInfo` (`instancing.rs:928`) already carries `height_index`, `vector_index`, `height_scale`,
`uv_transform`; the `Tessellation` push mirrors `DisplacePush` and adds the Phase-2/3 buffer handles and
the prev-frame inputs (§6, §8).

**Seam value agreement (watertightness link 2).** For a micro-vertex on a **seam edge**, read Phase 2's
per-edge seam flag + sampling mode: if the edge is flagged object-space/triplanar, sample the height in
object space (or via the triplanar projection) so the two incident triangles' differing UVs still yield
one value; otherwise the Phase-2 seam-aware dilation already guarantees a verified cross-seam texel match
and the ordinary UV sample suffices. Either way both sides read an **equal** height.

### 4. Normal + tangent from the full position Jacobian (watertightness link 3)

Emit a re-derived frame per micro-vertex, **not** the interpolated base normal and **not** the scalar
height-gradient normal `displace.slang:42` computes today. Differentiate the *displaced* surface:

- Base tangents from analytic Phong derivatives `∂P/∂u`, `∂P/∂v`.
- Scalar displacement `D = P + N(u,v)·h(uv)·scale`:
  `∂D/∂u = ∂P/∂u + (∂N/∂u)·h·scale + N·(∂h/∂u)·scale` (base curvature term `∂N/∂u` **plus** the
  displacement-derivative term — both, not just the height gradient).
- Vector displacement `D = P + M(u,v)·vdisp(uv)` (M the TBN): the derivative is the **full 3×3 Jacobian**
  of the vector field composed with `∂M/∂u,∂M/∂v` and the base tangents.
- `N' = normalize(cross(∂D/∂u, ∂D/∂v))` with orientation fixed against the geometric winding;
- `T'` = the base UV-tangent pushed through the same Jacobian, Gram-Schmidt-orthonormalized against `N'`,
  with the stored handedness `tangent.w` preserved (so mirrored UVs do not invert).

Both `N'` and `T'` are written into the micro-vertex (positions@0/normal@12/uv0@24/tangent@32 stride,
the `outVertices.Store` block at `displace.slang:45-50`). The fragment übershader is unchanged — it
consumes the same 48 B `Vertex`.

**Seam-consistent frame.** Boundary micro-vertices (§5) use Phase 2's shared per-welded-vertex
**direction** for `N'` and the seam-consistent **tangent seed** for `T'`, so both incident triangles emit
the identical frame at the shared edge — no shading seam and no lighting discontinuity across the weld.

### 5. Weld — snap boundary micro-vertices onto the interpolated base edge

The boundary micro-vertices of both incident triangles must be **bit-identical** in position, direction,
height, and tangent. Guarantee it by construction:

- **Position:** snap each boundary micro-vertex exactly onto the interpolated (Phong) base edge, whose
  curve is endpoint-only (§2), so both triangles place it at the same 3D point.
- **Direction + tangent:** use Phase 2's per-welded-vertex direction and seam-consistent tangent seed
  (spatial-hash weld output), not the per-triangle interpolated frame.
- **Height:** evaluate the same height at the same edge parameter via the §3 seam sampling mode.

This is welding-by-construction rather than a post-hoc average pass — the shared-edge micro-vertices are
never independently computed, so they cannot diverge. (Interior micro-vertices belong to exactly one
triangle and need no weld.)

### 6. Prev-position producer — identity-free, for motion vectors

Re-dicing changes vertex count and identity every frame, so there is no per-vertex history and the
previous frame's transient VB is already overwritten. Reconstruct each micro-vertex's **previous clip
position** identity-free: re-run the *same* dice+displace math at the *same* barycentric `(u,v)` sample
against the **previous** instance transform and **previous** height, then project by the previous
`view_proj`. Write it to a parallel per-micro-vertex **prev-position** transient buffer (acquired from the
Phase-1 keyed pool alongside the VB/IB), which Phase 5 wires into the motion pass (`aa.rs:370-371`).

This requires **retaining the previous per-instance transform + factors**: double-buffer the small
per-instance records (prev model matrix, prev `height_scale`, prev edge factors). The `Tessellation` push
carries `prev_model` and `prev_height_scale`; the prev edge factors come from Phase 3's double-buffered
factor buffer. Phase 5 owns the geomorph that keeps this smooth across integer factor transitions; Phase 4
is only the producer of the raw prev sample.

### 7. Skinning × displacement — read the skinned base, run after skinning

For a **skinned+displaced** instance the tessellator's base is the **skinned** geometry, not the static
mesh. Define the input binding so set-1 binding 0 points at the instance's slice of the `Skinning`
deformed ring (`Skinning::deformed_buffer(frame)`, `skinning.rs:184`) at its `deformed_offset` — the
already-skinned position/normal/tangent. Ordering falls out of the deform scope: the existing order is
morph→skin→displace (`renderer.rs:3782-3872`), so the skin pass writes the ring slice **before** the
tessellation pass reads it. The `Tessellation` pass declares `StorageReadCompute` on `deformed` for
skinned+displaced buckets, so the graph derives the skin-write→tess-read barrier automatically.

The ring is therefore **kept** for skinning/morph and, for skinned+displaced, used as the tessellator's
*input*; only the displacement *output* moves to the transient VB/IB. A skinned+displaced instance loses
skinning's cheap in-place BLAS UPDATE and takes the forced-BUILD tessellated path (Phase 7) — variable
output topology makes UPDATE illegal. That is stated, not hidden: a pure-skinned mesh still refits.

### 8. Amplitude — one world-space `height_scale` convention

Today `heightScale` is documented as "local-space displacement amplitude" (`displace.slang:35`), so a
scaled instance displaces by a scale-dependent world amount and the preview (unit sphere) and a scaled
scene mesh disagree. Pin `height_scale` as a **world-space length**. Output stays mesh-local (the
`DeformedRtInstance.world_transform = node model matrix` placement is a KEEP constraint), so the kernel
converts: the `Tessellation` push carries the instance's world scale (extracted from the model matrix);
the local-space amplitude is `height_scale / scale_along_direction`, so the final world displacement is
exactly `height_scale` world units regardless of instance scale. This must land **here**, before Phase 6
deletes the forced-`0.08` preview crutch — otherwise preview fidelity regresses. Non-uniform scale +
vector displacement transforms the direction rather than dividing by a scalar (see Risks).

### 9. Wiring into the deform scope + the transient VB/IB

Mirror `Displacement::wire_dispatches` / `record_displace` (`displacement.rs:101/146`):

- `Tessellation::wire_dispatches(frame, base_source, transient_vb, transient_ib, prev_vb, args,
  buckets)` resets the per-frame pool and, per bucket, allocates a set binding: base-in (static VB or
  skinned ring slice), the Phase-1 **keyed** transient VB/IB/prev-VB, the Phase-3 factor/offset buffers,
  and Phase-2 conditioning buffers (adjacency, per-welded-vertex direction/tangent, seam flags). The
  transient buffers are acquired via the Phase-1 keyed `acquire_buffer(frame, key, bytes, usage)` at a
  **fixed order every frame** so a skipped tessellation pass never desyncs the cursor for the Phase-9
  prism.
- `record_tessellate` binds the PSO + bindless set 0 once, then per bucket binds set 1, pushes `TessPush`,
  and issues `cmd_dispatch_indirect(args, offset_for_bucket)` (Phase 1's `add_indirect_compute_pass` /
  hand-recorded body).
- Replace the `if do_displace` block at `renderer.rs:3862` with `if do_tessellate`: an
  `RgPass::compute("tessellate")` declaring `StorageWriteCompute` on `transient_vb` / `transient_ib` /
  `prev_vb`, `StorageReadCompute` on the Phase-2/3 conditioning + factor/offset buffers, `IndirectCommandRead`
  on the Phase-3 args buffer (the `RgUsage` variant Phase 1 added), and — for skinned+displaced buckets —
  `StorageReadCompute` on `deformed`. Every downstream geometry consumer (Phase 6) declares
  `VertexInputRead` on `transient_vb` + `IndexInputRead` on `transient_ib`; the `tlas-build` pass (Phase 7)
  declares `AccelStructBuildRead` on both. The graph derives every barrier — no hand-written barrier, same
  as the existing displace pass.

### 10. Retire the 1:1 path (NO-LEGACY, same change)

Deleted when the tessellating path lands — not deferred:

- `assets/shaders/displace.slang` (the whole one-thread-per-vertex `computeMain`) and the
  `saffron_rendering::Displacement` subsystem's Displacement-mode dispatch — replaced by `tessellate.slang`
  + `Tessellation`. (`Displacement`'s *scalar-along-normal helper math* is lifted into `tessellate.slang`;
  the subsystem struct and its `wire_dispatches` reuse of the `Skinning` ring for Displacement-mode go.)
- `Displacement::wire_dispatches`' reuse of the `Skinning` deformed ring **as the Displacement output**
  (`displacement.rs:101` + the `displacement.wire_dispatches` call at `instancing.rs:638`).
- The `deformed_cursor += vertex_count` reservation **for Displacement buckets** (`instancing.rs:330-451`,
  specifically the `DisplaceBucket` / `displaced_rt` push at `instancing.rs:433/441` and the cursor bump at
  `instancing.rs:450`). Pure-Displacement buckets stop touching the ring entirely (base = static VB,
  output = transient); skinned+displaced still reserves a ring slice for its **skinned base** (skin writes
  it, tess reads it) but its amplified output goes to the transient VB/IB. Skinning and morph keep the ring
  and their cursor reservation unchanged.
- The `DeformedRtInstance` emitted for a displaced bucket (`instancing.rs:441`) is replaced by a
  tessellated-instance record carrying the generated index-buffer handle + GPU-written counts (consumed by
  Phase 7); the base-index reuse `inst.mesh.index_buffer()` at `rt.rs:430` and the static-index draw at
  `scene_pass.rs:102` / `scene_pass.rs:68` are the Phase-6/7 half of the same cutover.

A `set-tessellation` / inspect toggle replaces `set-displacement` as the master switch (Phase 9 adds the
quality/budget control command).

## Scope

`saffron-rendering` — the new `tessellate.slang` compute entry, the `saffron_rendering::Tessellation`
subsystem (set-1 layout + per-frame pools + `wire_dispatches` / `record_tessellate` /
`request_tessellate_pipeline`), the `renderer.rs` deform-scope pass, the `instancing.rs` bucket/reservation
cutover, and the deletion of `displace.slang` + the `Displacement` Displacement-mode path. Consumes Phase
1 (keyed acquire + `IndirectCommandRead` + `add_indirect_compute_pass`), Phase 2 (adjacency, weld
direction/tangent, seam flags, min-max pyramid), and Phase 3 (fractional factors, per-triangle offsets,
`cmd_dispatch_indirect` args, worst-case transient sizing). The 48 B tangent `Vertex` + `compute_tangents`
(B3) is the frame seed; the bindless set-0 height/vector sampling (`descriptors.rs:912`) is reused as-is.

## Depends on

`phase-3-edge-factors-allocation.md` (the fractional factors, per-triangle write offsets,
`cmd_dispatch_indirect` args, and worst-case-sized transient VB/IB this kernel writes into) — which in turn
depends on Phase 1 and Phase 2.

## Verification

- **Build/lint:** `just engine` then `just prepare-for-commit` clean on the touched crates; the deleted
  `displace.slang` / `Displacement` Displacement path leaves no dangling references (`clippy -D warnings`).
- **Crack-free shared edge (GPU-readback, the headline test):** a two-triangle patch whose shared edge is
  given **two different** fractional factors from its two sides must emit **bit-identical** boundary
  micro-vertices — read the transient VB back and assert the boundary positions/normals/tangents from both
  incident triangles match to the last bit. This proves the endpoint-only Phong edge curve + matched-gap
  stitch + weld actually close the seam.
- **Welded tangent continuity across a UV seam (GPU-readback):** on a seam-flagged edge, assert the
  emitted `N'` and `T'` (and the sampled height) agree on both sides — no shading discontinuity across the
  weld.
- **GPU-with-eyes (needs the NVIDIA card, `just run-engine`):** a **low-poly** authored-Displacement scene
  mesh shows a true bulged silhouette (not a bump); no cracks/light-leaks at seams under camera motion;
  validation-clean log. (Preview == scene and the RT silhouette parity are Phase 6 / Phase 7 acceptance,
  built on this buffer.)
- **Prev-position sanity (CPU + GPU):** with a static instance (no motion, same factors), the emitted
  prev clip position equals the current clip position per micro-vertex → zero motion-vector delta. (Phase 5
  owns the moving-camera ghosting test.)

## Risks

- **Full-Jacobian correctness is the quality wall.** Getting `N'`/`T'` from the full displaced-surface
  Jacobian (base curvature `∂N/∂u` **plus** the displacement derivative; full 3×3 for vector) is the whole
  reason lighting matches the silhouette. A scalar-gradient shortcut (what `displace.slang:42` does today)
  would ship faceted or mismatched normals on curved bases — must be resisted; it is the wrong, cheaper
  path.
- **Stitch topology is fiddly and is where cracks hide.** The matched-gap fan/staircase must leave **no**
  vertex on another triangle's edge interior across the full range of adjacent fractional factors; a single
  mismatched count reintroduces a T-junction and an RT light-leak. It is LUT-driven precisely so it is
  precision-exact, but the LUT is the part to crack-test hardest under camera motion.
- **Non-uniform instance scale.** The world-space amplitude convention (§8) is a clean scalar divide only
  under uniform scale; non-uniform scale must transform the displacement direction through the inverse-
  transpose (and for vector displacement, the whole TBN), or a scaled mesh's displacement skews. Pin the
  exact math here so preview and scene cannot diverge later.
- **Skinned+displaced ordering and cost.** The tess-read-after-skin ordering must hold every frame (it
  falls out of the deform-scope order + the declared `StorageReadCompute` on `deformed`), and such an
  instance necessarily pays a full per-frame BLAS BUILD (Phase 7). If the ordering is ever wrong the
  tessellator reads stale/un-skinned base geometry — a silent correctness bug, not a crash.
- **Prev-sample domain divergence.** When a large factor change makes a micro-vertex's barycentric domain
  appear/vanish between frames, the identity-free prev sample is approximate at that vertex; Phase 5's
  geomorph across integer transitions is required so the approximation stays sub-pixel and does not spike
  motion vectors. Phase 4 must retain the prev transform + prev factors faithfully or Phase 5 has nothing
  to reconstruct from.
- **Vector-field filtering.** Mip-filtering a tangent-space XYZ vector map can collapse the offset
  direction (average of opposing vectors → zero); sample the vector map at a controlled LOD and avoid
  naive trilinear averaging of the direction, or overhangs alias and thin features vanish.
- **NO-LEGACY blast radius.** Deleting `displace.slang` + the `Displacement` Displacement path, the ring
  reservation for displaced buckets, and the `DeformedRtInstance` displaced-bucket emit in one change
  touches `instancing.rs`, `renderer.rs`, `scene_pass.rs`, and `rt.rs` together (Phases 6/7 land the raster
  and RT halves). Sequence the cutover so the tree builds at the phase boundary — the tessellating path
  must be wired end-to-end before the 1:1 path is removed, not after.
