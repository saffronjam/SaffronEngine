# Phase B2 — in-scene compute displacement → shared buffer

**Status:** IMPLEMENTED (builds + `clippy --workspace -D warnings` + `fmt` + 185 rendering-lib GPU
tests + control/protocol tests all green; **on-screen visual** — silhouette + self-shadowing — still
wants a GPU-with-eyes run). The **displacement baseline** landed: a `displace` compute pre-pass
(`displace.slang`, new `saffron_rendering::Displacement` subsystem mirroring `Skinning`) displaces each
displacement-enabled instance's base vertices by the height field into the **shared deformed-vertex
buffer** that `Skinning` owns — non-overlapping cursor slices — so **every** geometry pass (scene, depth,
directional/spot shadow, **point-shadow cube**, GBuffer, motion) reads one already-displaced static mesh.
This fixes the cross-pass inconsistency the übershader VS path had (point shadows never displaced), and
the übershader's per-vertex displacement is now **retired** (one mechanism). Wiring: `Instancing`
detects a displacement bucket (`DisplaceInfo`), stamps it deformed (never merges), and wires cur+prev
dispatches through `Displacement::wire_dispatches` (prev writes the same static displacement → zero
deformation motion, pure object motion); the renderer resolves the two-set displace PSO (bindless set 0 +
the displace buffers set 1, sampling the height map by index) and adds an `RgPass::compute("displace")`
with `StorageWriteCompute` on `deformed`/`prev_deformed` in the deform scope — every existing
`VertexInputRead` consumer already covers it. A `set-displacement {0|1}` control command + DTO toggles the
path. **Deferred (documented follow-ups, not regressions):** adaptive *tessellation* (adding triangles
by screen-space edge factors) and **watertight welding** — the current pass displaces the base vertices
without subdividing; a densely-tessellated base (the preview sphere, terrain) already reads well, and the
adaptive/welding layer is the genuine research the section below details. In-scene POM (`FEATURE_HEIGHT`)
stays for non-displacement height materials.

## Build plan (concrete, grounded in the existing skinning subsystem)

The **compute-skinning deformed-buffer path is the exact template** — B2 is "skinning, but the per-vertex
transform is a height displacement." Mirror it:

1. **Compute prepass** (`displace.slang`, a new compute entry): dispatch one thread per output vertex of
   a displacement-flagged mesh. Read the base vertex (position/normal/uv), sample the height map at the
   tiled uv (`SampleLevel`), offset position along the normal by `height_scale`, recompute the normal by
   finite-difference of the height field, write the displaced `Vertex` into a **B1 transient buffer**
   (`TransientResources::acquire_buffer`, usage `STORAGE | VERTEX | SHADER_DEVICE_ADDRESS |
   ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR` — matching `make_deformed_buffer`). *Baseline =
   per-base-vertex displacement (no new triangles); adaptive subdivision (§ below) is a follow-up layer.*
2. **Render-graph wiring**: add the prepass via `add_compute_pass` declaring `StorageWriteCompute` on the
   transient buffer + `SampledReadCompute` on the height texture; the depth/shadow/GBuffer/main passes
   declare `VertexInputRead` on it. The graph derives the compute→raster barrier automatically (this is
   precisely why B1 imports the transient — no hand-written barrier).
3. **Draw path**: bind the displaced buffer instead of the base for displacement-flagged draws — mirror
   `deformed_buffer(frame)` selection in `renderer.rs` (~3732) and the scene-pass vertex-buffer bind.
4. **Übershader / material**: drop `FEATURE_DISPLACE` VS displacement for *in-scene* draws (the compute
   prepass already moved the vertices — double-displacing is the bug to avoid); keep the `mesh.slang`
   height-gradient shading normal. **Retire in-scene POM** (`FEATURE_HEIGHT` parallax) for
   displacement-enabled `.smat` — delete the `parallaxUv` branch's reachability for those materials.
5. **`sa` command**: a `set-displacement`/inspect toggle so the path is scriptable (per "keep current").

### Adaptive watertight tessellation (the second layer, the genuine research)

On top of the displacement baseline: derive **per-edge** screen-space subdivision factors that are
*identical from both patches sharing an edge* (evaluate the factor from the two endpoint positions only,
never per-triangle interior) → no T-junction cracks; tessellate barycentrically (Phong) into the
transient buffer with an index buffer; **weld** UV-seam/boundary vertices (average position+normal across
duplicates keyed by spatial hash) before rasterizing. This is open questions #1/#3/#6 and the ~0.5 ms
target; land it only with GPU crack-testing under camera motion.

**Scope:** `saffron-rendering` (compute prepass, render graph, mesh übershader)
**Depends on:** phase-b1 (transient buffer) — **DONE**; **`primitive-meshes/`** (clean-UV base meshes)

## Goal

The modern-correct, portable in-scene displacement path: a render-graph **compute prepass** that, per
frame, computes screen-space-adaptive **watertight** edge factors, tessellates a scene mesh into a
scratch vertex/index buffer, displaces (scalar height), welds seams, and recomputes normals/tangents.
Rasterizer, shadow passes, and GBuffer all consume the same buffer. Retire in-scene POM for
displacement-enabled `.smat` materials.

## Why this over hardware tessellation

The stored-buffer model enables **edge welding** (averaging positions/normals across shared edges/UV
seams before rasterizing) — impossible with hardware tessellation's immediate-consumption model — and,
crucially, the buffer can back a **BLAS** (Phase C1). Reported ~0.5 ms adaptive vs ~15 ms fixed
tessellation-127 (Filmic Worlds). One tessellation shared by all passes, no sub-pixel micro-triangle
rasterizer inefficiency.

## Touch points

- **Compute prepass** — per-edge screen-space factor derivation (identical from both patches sharing an
  edge → no cracks); tessellate into the transient buffer (Phong or similar barycentric scheme);
  displace along the interpolated normal by the height field.
- **Welding pass** — average seam vertices; recompute normals/tangents (analytic finite-difference).
- **Render graph** — declare the transient buffer (phase-b1), derive barriers; depth/shadow/GBuffer read
  it.
- **Übershader / material** — a displacement feature bit gating displacement-enabled `.smat` materials;
  the in-scene POM path is retired for those.

## Verification

- A displaced plane/terrain shows true silhouettes in the main viewport and in shadow maps
  (self-shadowing matches the displaced surface); validation-clean; frame-cost measured.
- No cracks at UV seams / mesh boundaries under camera motion (LOD transitions watertight).

## Risks (the hard part)

- **Watertight adaptive factors across UV seams + mesh boundaries** — budget welding; possibly authored
  seam-height normalization (open question #1).
- **Per-frame cost vs animation-static reuse** — caching displaced buffers, interaction with instancing
  and the PSO/übershader cache (open questions #3, #6).
- **`height_scale` world-space units** — consistent with the Phase A preview (open question #2).
