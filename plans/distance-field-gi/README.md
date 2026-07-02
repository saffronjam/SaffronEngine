# Distance-field GI — per-mesh MDF + Global SDF clipmap + DDGI sky-on-miss

**Status:** COMPLETED

The skybox/IBL over-lights enclosed interiors — the Sponza second floor stays bright when it
should be in shadow. A first attempt (per-mesh SDF DFAO sampled directly per pixel) now runs but
looks wrong: smeary low-frequency "stain" blobs on flat walls, zebra/moiré on the open roof, and a
1.25 ms prepass. The cause is that we built only half of UE5's system. UE composites per-mesh Mesh
Distance Fields (MDF) into a camera-centered Global Distance Field (GDF) clipmap, traces that as one
field, denoises it, and combines it with SSAO — and the UE5 default is Lumen, where the skylight is
occluded simply because indirect rays that **miss** reach the sky. We have per-mesh fields, sampled
directly, with no global composition and a field too coarse for Sponza-as-one-mesh — the one
configuration that looks bad.

## Intended outcome

Complete the system the UE5 way, all phases: per-mesh MDF, composited into a Global SDF clipmap, with
DDGI carrying indirect light and the sky entering only on ray-miss. Interiors correctly dark, exterior
walls lit, occlusion directional, no zebra, no import freeze, and one unified indirect path instead of
two half-systems.

## Settled decisions (do not re-litigate)

- **Bake = GPU jump-flood, at mesh-upload time, cached.** When a `GpuMesh` is first built: voxelize to
  a seed volume on the GPU, JFA propagates the nearest surface, derive unsigned distance, **sign** it
  (flood an outside-seed, cross-checked by the nearest-triangle normal), compress to sparse bricks.
  Runs in ms; cached to a sidecar `assets/cache/<meshHash>.sdf` so later loads skip the bake. Import
  stays GPU-free (`crates/assets/src/import.rs` keeps its no-GPU contract). The CPU
  `compute_sdf_for_mesh` import bake and dense SDST v1 are **removed** (NO LEGACY).
  `MeshBvh::nearest_signed_distance` stays as the SDST format unit-test oracle.
- **SDST v2 = header + indirection volume + brick atlas.** Indirection volume is `R32_UINT`, one
  texel per 8³ brick mapping to an atlas brick base or `EMPTY`. Brick atlas is `Image3D R16_SNORM`,
  bricks of 8³ (7 unique + 1 shared border for seam-continuous trilinear). `sdfDistance`:
  world → local → brick coord → nearest indirection lookup → in-brick UVW (with border) → one
  trilinear tap. Empty bricks return the coarse coverage distance (big step).
- **GDF leak mitigation.** DDGI rays trace the fine per-mesh MDF for the near field (~first 2 m) and
  the GDF only beyond; DDGI probe spacing is set below wall thickness (camera-centered clipmap, ~1.5 m
  innermost); the Chebyshev moment leak test is kept.

## Honest caveats

- **Cost-neutral on Sponza until P6.** This is a quality + architecture investment; P6 is where DDGI
  replaces the prepass and the 1.25 ms `sdf-ao` cost comes back. Set expectations accordingly.
- **The GDF adds no spatial detail for one-giant-mesh content.** Sponza imports as one 262k-tri mesh;
  the blob fix comes from the densified per-mesh MDF (P2), not the GDF. The GDF's wins (alias-free
  O(1) tracing, cost decoupling) land on modular content and at scale.
- **GPU-bake sign correctness is the central new risk.** Signing from JFA on non-watertight Sponza is
  unproven — validate against the CPU `nearest_signed_distance` oracle (unit sphere/box + a Sponza
  spot-check) before trusting it.

## Phases (dependency-ordered)

| Phase | Goal |
|-------|------|
| [Phase 1 — DFAO trace + denoiser](phase-1-dfao-trace-and-denoiser.md) | Fix the zebra now: solid-angle hemisphere cones, world-space self-shadow bias, a signal-correct `sdf_ao_accum` denoiser. |
| [Phase 2 — GPU JFA bake + bricks](phase-2-gpu-jfa-bake-and-bricks.md) | Fix the blobs: GPU jump-flood MDF bake, sparse SDST v2 bricks, sidecar cache, per-asset resolution scale; remove the CPU import bake. |
| [Phase 3 — Mip prefilter + coverage](phase-3-mip-prefilter-and-coverage.md) | Anti-alias at range and skip empty space: 3-mip prefiltered distance + coarse coverage volume, cone-footprint mip select. |
| [Phase 4 — Global SDF clipmap](phase-4-global-sdf-clipmap.md) | Shared, alias-free oracle: camera-centered 3-cascade clipmap, cull + composite of MDF bricks, toroidal incremental updates, near/far trace handoff. |
| [Phase 5 — DDGI on the GDF, sky-on-miss](phase-5-ddgi-on-gdf-sky-on-miss.md) | Interiors dark: DDGI rays sphere-march the GDF, sky enters only on miss, camera-centered probe clipmap; delete the solid-AABB voxel proxy; default ON. |
| [Phase 6 — Retire the prepass, reconcile](phase-6-retire-prepass-reconcile.md) | Remove the 1.25 ms: delete the per-pixel DFAO prepass; indirect occlusion = DDGI ray-miss + small-radius GTAO; one per-pixel SDF consumer left (reflection occlusion on the GDF). |

## Reuse vs delete

- **Keep/extend:** `MeshBvh` (picking + SDST format oracle); the bindless 3D atlas + sampler
  (`crates/rendering/src/descriptors.rs`, `resources.rs`) → brick atlas + indirection; the DDGI probe
  machinery; the `lighting.slang` DDGI replace-by-coverage piece (the correct Lumen-style part); GTAO
  as the contact multiply; `view_target.rs` history/motion for the denoiser.
- **Delete (NO LEGACY):** CPU import-time SDST bake + dense SDST v1; the solid-AABB `ddgi_voxelize`
  proxy + box buffer; eventually `sdf_ao.slang` + the stacked `sky.ao` path.

## Verify per phase

- `just engine` clean + `just prepare-for-commit` (fmt + clippy `-D warnings`).
- P2 bake: GPU bake of Sponza completes in **ms** with no import freeze; SDST v2 round-trip +
  sphere/box analytic-distance + sign unit tests against the CPU oracle; sidecar cache reused.
- Visual check on the **NVIDIA path** (`just run`, Sponza) at each phase boundary — the gate cannot
  judge look: P1 zebra gone on a flat roof, P2 wall blobs resolve into real geometry, P5 interior dark
  with exterior lit and directional, no flicker on drag, DDGI-off still reasonable.
- Profiler each phase; confirm P6 removes the `sdf-ao` 1.25 ms and the frame is ≤ baseline.
- `gen-protocol` + `cd editor && bun run check` for any control surface change.
