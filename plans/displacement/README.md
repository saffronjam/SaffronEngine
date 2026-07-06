# Displacement & height maps — research track + phased plan

**Status:** IN PROGRESS — two tracks.

- **Geometry engine (A/B/C): A, B1, B2, B3, C1, C2 IMPLEMENTED** (build + `clippy -D warnings` on the
  touched crates + `fmt` + geometry/control/protocol tests green; on-screen visuals still want a
  GPU-with-eyes pass). The in-scene displacement system is built: a `displace` compute pre-pass bakes the
  height field into the shared deformed-vertex buffer that **every** pass reads (Phase A's übershader VS
  path is retired — one mechanism), and the RT BLAS refits over that buffer for free (C1). **B3** (vector
  displacement) is **built**: the engine's `Vertex` gained the UV-aligned per-vertex tangent it lacked
  (Lengyel's method on import, 48 B stride, `.smesh` v4, carried through the skin/morph/displace compute
  kernels), and on top of it a `vector_displacement_texture` switches the displace pre-pass from
  scalar-along-normal to tangent-space XYZ through the TBN (overhangs). **C2** (the `VK_EXT_mesh_shader`
  front end) is **built end-to-end** — meshlet clustering (`build_meshlets`, unit-tested), device support
  (extension + feature + `cmd_draw_mesh_tasks` dispatch), GPU meshlet-buffer upload, the task+mesh shaders
  (`meshlet.slang`), the graphics PSO (`Pipelines::request_meshlet`), and the per-frame set-8 pools +
  `cmd_draw_mesh_tasks` scene-pass draw (`MeshletRaster`). It is gated behind `mesh_shader_supported()`
  **and** the `SAFFRON_MESH_SHADER` opt-in, so the validated index-draw path stays the default; the one
  remaining step is *running* it on the NVIDIA card (`SAFFRON_MESH_SHADER=1 just run`) for a visual +
  validation-layer confirmation (llvmpipe advertises no mesh shaders, so the path never engages here) —
  a zero-risk check, not a code gap. **Deferred within B2:** adaptive tessellation + watertight welding
  (documented follow-ups; the baseline displaces the base vertices without subdividing).
- **Material-facing track (D1–D3): IMPLEMENTED** (engine + editor compile; `clippy -D warnings` clean on
  the touched crates; unit/serde/golden coverage green — full GPU e2e deferred to the planned test
  rehaul). Grounded in a **2026 cross-engine survey** (below). D1 replaced the `displacement: bool` with a
  `saffron_core::HeightMode` enum (Bump / Parallax / Displacement — the Unity-HDRP one-map-+-mode shape,
  new `FEATURE_HEIGHT_BUMP` shader path, `heightMode` on the wire + a Material-editor dropdown); D2 routes
  an imported PBR map to the right mode by provider label (`detect_height_mode`) and closes the **Rock 063
  swimming** defect (a Displacement map imports in real-displacement mode, previewed on the dense sphere);
  D3 rewrote `parallaxUv` with offset-limiting + a dynamic sample count so the Parallax mode no longer
  swims at grazing angles.

"True" displacement means **new vertices exist at displaced positions** — real silhouettes, and the
detail is visible to shadow maps, the RT BVH, and GI. That is the line separating it from the engine's
faux depth: `parallaxUv` in `lighting.slang` (a 24-step POM ray-march driven by the `.smat`
`height_scale`) — per-pixel illusion, smooth silhouette, nothing in shadows or the BVH.

The two tracks are complementary: A/B/C build the **geometry** (how a displaced vertex reaches every
pass + the BVH); D1–D3 build the **material surface** (how a user/importer *chooses* Bump vs Parallax
vs Displacement, and makes each mode look right). The geometry track splits further into a **near-term
preview slice** (Phase A — feeds `texture-material-previews/` and `material-graph-live-preview/`) and the
**in-scene system** (Phases B–C — the real engineering cost).

## The motivating defect (Rock 063)

Importing an ambientCG material wires its **Displacement** map into the height slot but leaves the mode
at the POM default, so the preview sphere **swims/melts at grazing angles** — POM offset
`(vt.xy / max(|vt.z|,0.1)) * scale` blows up as `vt.z→0` — while ambientCG's reference render is clean
(real displacement). Worse, the same map is treated three ways across our surfaces: the interactive
*texture* preview forces real displacement (`commands_asset.rs:3059`), but the thumbnail
(`thumbnail.rs:469`) and the *material* preview/import use POM. The D track fixes both the wrong default
(D2), the missing mode vocabulary (D1), and the POM artifact itself (D3).

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

## Cross-engine survey (2026) — the material-facing verdict (drives the D track)

A live web survey of how shipping engines expose a grayscale height/displacement map to *materials*
(full report + ~40 sources in the research dossier):

| Engine/Tool | Default for a height map | Mode vocabulary | Import auto-wires? |
|---|---|---|---|
| Unreal 5.4–5.8 | inert (Nanite Tessellation ships **disabled**) | POM function / Nanite `Displacement` / WPO | no — height is "opt-in" |
| **Unity HDRP** | **None** | `Displacement Mode`: None / Vertex / **Pixel (POM)** / Tessellation | no |
| Godot 4 | off (`heightmap_enabled=false`) | Height: offset parallax / Deep-Parallax POM (**never geometry**) | no |
| Blender | **Bump Only** | Bump Only / Displacement Only / Displacement+Bump | Node Wrangler → Bump; Poly Haven add-on → real displacement |
| **three.js** ⭐ | **real per-vertex displacement immediately** (`displacementScale=1`) | `displacementMap` (geometry) vs `bumpMap`/`normalMap` | code-assign = full displacement |
| CryEngine | off until a shader-gen flag | Offset Bump / POM / **Silhouette POM** | `_displ` map auto-loads with the flag |

**What it means for us:**

- **One map + a mode selector is the universal shape.** No engine ships "height" and "displacement" as
  distinct file types; the *technique* decides. → **D1**: adopt Unity-HDRP's one-map-+-`HeightMode`
  enum (Bump / Parallax / Displacement), replacing our `displacement: bool` and adding the missing
  Bump mode.
- **The big editor engines default a height map to *non-geometric* (bump/POM); displacement is
  opt-in. three.js — our closest architectural peer — is the lone auto-displacer.** So "work like the
  major engines" and "honor the library's authoring intent" pull opposite ways. → **D2** resolves it by
  **routing on the provider map label** (library *Displacement* map → Displacement mode; *Height* map →
  Parallax; else Bump), with a per-material override — a deliberate, documented divergence toward the
  three.js/library-intent default because it fixes Rock 063.
- **POM's grazing "swim" is mitigated, never solved, by POM alone** (offset-limiting, dynamic sample
  count, distance fade; only real geometry or CryEngine's Silhouette POM fixes the outline). → **D3**.
- **Hardware tessellation "removed" is Unreal-only spin** — Vulkan 1.4 keeps tessellation shaders and
  mesh shaders are the modern GPU displacement path. This *confirms* the geometry track's B/C direction
  (compute → buffer → BLAS now; mesh shaders in C2), and the RT-BLAS-over-displaced-buffer concern the
  survey raises is **already handled by C1**.

## Ordering

**A** (pre-subdivided preview sphere + VS displacement; retire preview POM) → **B** (compute adaptive
tessellation → graph-managed scratch buffer, co-developed with the missing *transient graph resources*
facility; scalar then vector) → **C** (BLAS over the displaced buffer for RT; later, an optional
task+mesh-shader front end). **Skip fixed-function tessellation and DMM entirely.**

Why: Phase A unblocks the preview plans immediately with no new Vulkan features and a tiny blast radius;
the primitive-mesh facility (`plans/primitive-meshes/`) is its shared prerequisite; Phase B is where the
real cost lives and it wants the transient-resource work anyway; Phase C gates on the RT-BLAS refit
design and, optionally, a mesh-shader roadmap that does not yet exist.

**Material-facing track:** **D1** (the `HeightMode` enum — the foundation the others need) → **D2**
(import routing + default + one mode across every preview surface; closes Rock 063) → **D3** (POM
robustness; standalone, can land anytime). D1 depends only on the A/B feature bits already implemented,
so the whole D track is buildable **now** — it does not wait on the deferred B2 tessellation layer
(the dense preview sphere already shows real displacement; low-poly scene meshes fall back to
Bump/Parallax until the adaptive-tessellation layer lands).

## Phases

**Geometry engine (A/B/C):**

- **`phase-a-preview-vs-displacement.md`** — pre-subdivided base + VS scalar displacement; analytic
  TBN; retire preview POM. *Depends on `primitive-meshes/`. Near-term product slice.*
- **`phase-b1-transient-graph-resources.md`** — graph-managed scratch buffer facility (an engine
  capability gap: AGENTS.md "not yet").
- **`phase-b2-compute-adaptive-tessellation.md`** — compute prepass → buffer; watertight factors;
  welding; retire in-scene POM.
- **`phase-b3-vector-displacement.md`** — tangent-space XYZ (overhangs/undercuts).
- **`phase-c1-rt-blas-over-displaced-buffer.md`** — build/refit BLAS over the displaced buffer.
- **`phase-c2-mesh-shader-frontend.md`** — task+mesh (`VK_EXT_mesh_shader`) amplification front end, built
  end-to-end + gated behind `mesh_shader_supported()` + `SAFFRON_MESH_SHADER` (default-off; awaits a
  visual pass on mesh-shader hardware).

**Material-facing track (D):**

- **`phase-d1-height-mode-enum.md`** — replace `displacement: bool` with a `HeightMode` enum
  (Bump / Parallax / Displacement) over one Height Map; add the missing Bump mode + inspector UI.
- **`phase-d2-import-routing-and-default.md`** — route an imported PBR height/displacement map to the
  right mode by provider label; fix the Rock 063 default; unify thumbnail ↔ preview ↔ material.
- **`phase-d3-pom-robustness.md`** — offset-limiting + dynamic sample count + distance fade so the
  Parallax mode stops swimming at grazing angles.

## Dependencies & cross-plan notes

- Phase A ↔ `texture-material-previews/phase-5` and `material-graph-live-preview` — Phase A *is* the
  height-preview mechanism. If Phase A lands before the previews' thumbnail work, use VS-displacement in
  the thumbnail too and **skip POM entirely** (NO-LEGACY — don't ship then delete a POM path).
- Phase B needs `phase-b1` (transient resources) — an unbuilt engine feature, tracked here but a genuine
  capability gap of its own.
- The **primitive-mesh facility** (`plans/primitive-meshes/`) owns the base-mesh generators; this plan
  does **not** duplicate them.
- **D1 → D2**: D2's routing needs D1's mode vocabulary (and its Bump fallback). **D3 is independent** —
  it fixes `parallaxUv` and can land before or after D1/D2. The D track builds on the *already
  implemented* A/B feature bits (`FEATURE_HEIGHT`/`FEATURE_DISPLACE`), so it does **not** block on the
  deferred B2 adaptive-tessellation/welding layer.
- **D2 ↔ the storefront** (`editor/src/storefront/`, `src-tauri/connectors/`) — the provider map label
  ("Displacement" vs "Height") is the routing signal; it must reach the host import, not be flattened to
  a generic height role in `detect_material_role`.
- **D2 ↔ `texture-material-previews/` + `material-graph-live-preview/`** — D2 unifies the thumbnail,
  interactive-texture, interactive-material, and imported-material surfaces onto one mode rule; those
  plans' preview wiring is the same host path.

## Key sources

Industry shift & tessellation: Niessner survey (niessnerlab.org/papers/2015/6survey), Frostbite adaptive
terrain, GPU Gems 2 ch.7, WATER/DiagSplit, UE5 WPO-replaces-tessellation forum thread.
Mesh shaders: `VK_EXT_mesh_shader` proposal (Khronos), GPUOpen vertex→mesh, Khronos mesh-shading blog.
Nanite: UE Nanite docs + Nanite Tessellation (5.3+), graphicrants "Nanite Tessellation", Wihlidal deep
dive. RT displacement: nvpro `vk_displacement_micromaps` (archived), RTX Mega Geometry blog +
`vk_tessellated_clusters`, Projective Displacement Mapping (arXiv 2502.02011). Compute baseline: Filmic
Worlds "Adaptive Compute Tessellation" (~0.5 ms figure), Cyanilux vertex displacement. (Full URL list in
the source dossier.)

**Material-facing track (D) — 2026 cross-engine survey** (one-map-+-mode, defaults, POM mitigations;
~40 sources): Unity HDRP `Displacement Mode` docs + `Lit.shader` source; Godot `BaseMaterial3D` height
feature; Blender displacement modes; three.js `MeshStandardMaterial` (`displacementScale=1`) + Babylon
`useParallax`/`useParallaxOcclusion` + Filament (no height feature); CryEngine Silhouette POM +
Policarpo/Oliveira relief lineage; Quixel Bridge / ambientCG / Poly Haven import behaviour; POM
mitigations — Welsh 2003 (offset limiting), Tatarchuk SIGGRAPH 2006 (dynamic sample count), LearnOpenGL
Parallax Mapping. Full report with URLs in the research dossier.
