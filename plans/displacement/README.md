# True geometric displacement — research track + phased plan

**Status:** NOT STARTED (Phase A is near-term product; Phases B–C are long-horizon research/build)

"True" displacement means **new vertices exist at displaced positions** — real silhouettes, and the
detail is visible to shadow maps, the RT BVH, and GI. That is the line separating this from the
engine's current faux depth: `parallaxUv` in `lighting.slang` (a 24-step POM ray-march driven by the
`.smat` `height_scale`) — per-pixel illusion, smooth silhouette, nothing in shadows or the BVH.

This plan splits into a **near-term preview slice** (Phase A — feeds `texture-material-previews/` and
`material-graph-live-preview/`) and the **in-scene system** (Phases B–C — the real engineering cost).

## Verdict from the survey

| Technique | RT / BVH fit | Verdict |
|---|---|---|
| Hardware tessellation (TCS/TES) | transient output → needs a capture pass | **Skip** — UE5 *removed* it; sub-pixel micro-triangle inefficiency; poor RT fit; legacy the moment written |
| Mesh + task shaders | transient → no BVH | **Later** — when a mesh-shader/GPU-driven roadmap exists (AGENTS.md "not yet") |
| Nanite virtualized geometry | rides its own pipeline | **Not portable** — ideas only (compressed 2D height, pixel-density tessellation) |
| DMM (`VK_NV_displacement_micromap`) | native compact BVH | **Skip** — deprecated/archived, NV-only |
| RTX Mega Geometry clusters | native, fast rebuild | **Watch** — NV-only today; may go cross-vendor like Opacity Micromaps did |
| **Compute tess → buffer** | **buffer → BLAS (portable)** | **Phase B target** — ~0.5 ms adaptive vs ~15 ms fixed; welds seams; feeds RT |
| **Pre-subdivided + VS displace** | verts exist → BLAS-able | **Phase A target** — trivial, no new Vulkan features, exact for a fixed-distance preview |
| Baked displaced mesh | verts exist | Not a live-material fit |

**Guiding principle (AGENTS.md modern-correct / no-legacy):** don't build fixed-function TCS/TES (Unreal
removed it; bad RT fit) and don't invest in DMM (dead). Prefer compute/mesh-shader tessellation writing
to a **buffer that also feeds the BLAS**. Split the effort so the previews get real displacement long
before the general system exists.

## Ordering

**A** (pre-subdivided preview sphere + VS displacement; retire preview POM) → **B** (compute adaptive
tessellation → graph-managed scratch buffer, co-developed with the missing *transient graph resources*
facility; scalar then vector) → **C** (BLAS over the displaced buffer for RT; later, an optional
task+mesh-shader front end). **Skip fixed-function tessellation and DMM entirely.**

Why: Phase A unblocks the preview plans immediately with no new Vulkan features and a tiny blast radius;
the primitive-mesh facility (`plans/primitive-meshes/`) is its shared prerequisite; Phase B is where the
real cost lives and it wants the transient-resource work anyway; Phase C gates on the RT-BLAS refit
design and, optionally, a mesh-shader roadmap that does not yet exist.

## Phases

- **`phase-a-preview-vs-displacement.md`** — pre-subdivided base + VS scalar displacement; analytic
  TBN; retire preview POM. *Depends on `primitive-meshes/`. Near-term product slice.*
- **`phase-b1-transient-graph-resources.md`** — graph-managed scratch buffer facility (an engine
  capability gap: AGENTS.md "not yet").
- **`phase-b2-compute-adaptive-tessellation.md`** — compute prepass → buffer; watertight factors;
  welding; retire in-scene POM.
- **`phase-b3-vector-displacement.md`** — tangent-space XYZ (overhangs/undercuts).
- **`phase-c1-rt-blas-over-displaced-buffer.md`** — build/refit BLAS over the displaced buffer.
- **`phase-c2-mesh-shader-frontend.md`** — optional, roadmap-gated task+mesh amplification front end.

## Dependencies & cross-plan notes

- Phase A ↔ `texture-material-previews/phase-5` and `material-graph-live-preview` — Phase A *is* the
  height-preview mechanism. If Phase A lands before the previews' thumbnail work, use VS-displacement in
  the thumbnail too and **skip POM entirely** (NO-LEGACY — don't ship then delete a POM path).
- Phase B needs `phase-b1` (transient resources) — an unbuilt engine feature, tracked here but a genuine
  capability gap of its own.
- The **primitive-mesh facility** (`plans/primitive-meshes/`) owns the base-mesh generators; this plan
  does **not** duplicate them.

## Key sources

Industry shift & tessellation: Niessner survey (niessnerlab.org/papers/2015/6survey), Frostbite adaptive
terrain, GPU Gems 2 ch.7, WATER/DiagSplit, UE5 WPO-replaces-tessellation forum thread.
Mesh shaders: `VK_EXT_mesh_shader` proposal (Khronos), GPUOpen vertex→mesh, Khronos mesh-shading blog.
Nanite: UE Nanite docs + Nanite Tessellation (5.3+), graphicrants "Nanite Tessellation", Wihlidal deep
dive. RT displacement: nvpro `vk_displacement_micromaps` (archived), RTX Mega Geometry blog +
`vk_tessellated_clusters`, Projective Displacement Mapping (arXiv 2502.02011). Compute baseline: Filmic
Worlds "Adaptive Compute Tessellation" (~0.5 ms figure), Cyanilux vertex displacement. (Full URL list in
the source dossier.)
