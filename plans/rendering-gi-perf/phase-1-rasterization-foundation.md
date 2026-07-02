# Phase 1 — Rasterization foundation

**Status:** COMPLETED — 1.1 (MSAA 1×) + 1.2 (depth prepass w/ alpha-clip fragment) + 1.3 (per-material
backface culling, end-to-end two-sided support) all DONE + verified. **`scene-opaque` 2.14 → 0.67 ms**
(1.1+1.2: →0.92; 1.3: →0.67). Foliage/curtains intact, solids no holes (winding correct),
validation-clean, fmt+clippy clean.

Cut the raw fragment/bandwidth cost of `scene-opaque` (2.14 ms) with the three rasterization-level
wins, and establish the clean depth + reliable overdraw-free opaque pass that Phase 2's screen-space
GI resolve depends on.

## Changes

### 1.1 — MSAA 4× → 1×
- **Where:** `engine/crates/host/src/lib.rs` — `renderer.set_aa(4, false, false)` in `on_create`.
- **Change:** `set_aa(1, false, false)`. TAA remains the antialiaser (independent `taa` pass).
- **Why:** running 4× MSAA *and* TAA is redundant; 4× multiplies attachment bandwidth + edge-fragment
  shading on a high-silhouette 262k-tri mesh. Modern forward+ with TAA runs 1× MSAA.
- **Watch:** confirm no code path assumes `sample_count > 1` (MSAA resolve is skipped at 1×; the
  depth-prepass + scene PSO bake `sample_count`, so they follow automatically). Verify edges are
  acceptable with TAA alone.

### 1.2 — Enable the depth prepass
- **Where:** `engine/crates/rendering/src/renderer.rs` — `use_depth_prepass: false` default (~918);
  prepass is already fully wired (`record_depth_prepass`, the `depth-prepass` RgPass ~4020-4050,
  gated on `pipelines.depth_prepass.is_some()`).
- **Change:** default `use_depth_prepass: true`; ensure `pipelines.depth_prepass` is populated when
  it is on; make the scene opaque PSO use depth compare `EQUAL` (write off) when a prepass ran, else
  `LESS` (write on) — one PSO key bit, or a runtime dynamic-state compare op.
- **Alpha-masked materials:** must run their `discard` in the prepass too, or their pixels fail the
  `EQUAL` test in the scene pass. Confirm the depth-prepass shader samples albedo + discards for
  masked materials (or exclude masked draws from `EQUAL` and keep them `LEQUAL`).
- **Why:** one 262k-tri mesh with no prepass = full overdraw through the fat fragment. Prepass shades
  each pixel once. Expected `scene-opaque` 2.14 → ~1.0–1.4 ms net (after ~0.1–0.2 ms prepass).

### 1.3 — Backface culling with per-material two-sided
> **Scope found bigger than a flag; WIP.** Two facts surfaced while implementing:
> 1. **Perf payoff is marginal** — 1.2's depth prepass + early-Z already rejects back faces before the
>    fat GI fragment runs, so culling saves only back-face *rasterization*, not shading.
> 2. **It's an end-to-end feature, not a flag** — `double_sided` is recorded by the glTF importer but
>    **dropped at the scene-material layer**: the scene `MaterialSet`/`MaterialSlot` components (and
>    their project-JSON format) don't carry it, so it never reaches the renderer. Correct culling needs
>    per-submesh dynamic `vkCmdSetCullMode` (one-mesh Sponza is a single DrawItem with per-submesh
>    materials — a PSO-key bit can't express mixed single/double-sided submeshes), which requires
>    `double_sided` threaded through the scene component + JSON format + import, then a **re-import of
>    the test scene** (existing saved projects load it as `false` — a fresh-project migration, per the
>    no-migration rule).
>
> **DONE — end-to-end.** `double_sided` now flows: glTF import → scene `Material`/`MaterialSlot`
> component (+ project-JSON `doubleSided`, serde-default false) → `render_material` resolve →
> `SubmeshMaterial.double_sided` → `instancing::submesh_cull_for` → `DrawBatch.submesh_cull` →
> `record_scene_draw_list` sets per-submesh `vkCmdSetCullMode` (scene PSO has `VK_DYNAMIC_STATE_CULL_MODE`;
> other passes keep baked `NONE`). Two-sided → `NONE`, else `BACK`.
>
> **Verified:** `scene-opaque` 0.92 → **0.67 ms** (back-face rasterization *did* cost ~0.25 ms — the
> earlier "prepass fully subsumes it" estimate was too pessimistic); solids render with no holes
> (winding is CCW-correct); validation-clean (dynamic cull mode supported); fmt+clippy clean.
>
> **Caveat (no-migration):** projects saved before this load with `double_sided=false` everywhere, so
> their two-sided materials (Sponza curtains/foliage) cull `BACK` and show one-sided *from behind* until
> re-imported — a fresh-import migration, not a code path. A re-import (or a new project) gets it right.
> Not forcing a re-import of the user's `dev` project (would overwrite their saved materials).

- **Where:** `engine/crates/rendering/src/pipelines.rs` (~1148) `cull_mode(vk::CullModeFlags::NONE)`,
  hardcoded in the PSO key path.
- **Change:** add a `two_sided: bool` bit to the scene PSO key; `cull_mode = if two_sided { NONE }
  else { BACK }`. Drive `two_sided` from the material (glTF `doubleSided`, and the `.smat` flag) —
  Sponza's curtains/foliage are authored double-sided and must stay `NONE`; solid stone/floor cull
  `BACK`. Plumb the flag: material → draw batch key → PSO key.
- **Why:** culling back faces ~halves shaded fragments on closed geometry; stacks with 1.2.
- **Watch:** a mis-flagged solid mesh culling the wrong winding shows as holes; a mis-flagged thin
  mesh (curtain) culled shows one-sided. Verify curtains/foliage render from both sides.

## Verification
- `just engine` + `just prepare-for-commit` clean.
- GPU-timestamp profiler (debug OK for GPU times): `scene-opaque` ms before/after each sub-change;
  expect a large drop from 1.2 + 1.3, small overhead from the prepass pass itself.
- Screenshot A/B on `dev`: identical lit result (aside from AA), **curtains + foliage intact from
  both sides**, no z-fighting / missing surfaces from the `EQUAL` depth test, no holes from culling.
- Confirm validation-clean.
