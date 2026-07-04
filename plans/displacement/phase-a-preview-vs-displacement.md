# Phase A — displaced preview via pre-subdivided base + VS displacement

**Status:** NOT STARTED
**Scope:** `saffron-geometry`, `saffron-rendering` (`preview.slang`, `thumbnail_render.rs`),
`saffron-control` (`furnish_preview_scene`)
**Depends on:** **`primitive-meshes/`** (a subdividable sphere/plane base)

This is the **near-term product slice** — it gives `texture-material-previews/phase-5` and the material
graph preview a real displaced sphere with zero new Vulkan features. It replaces POM *for the preview*.

## Goal

A pre-subdivided base mesh whose vertex shader samples the `.smat` height texture and offsets each
vertex along its normal by `height_scale` (reusing `uv_tiling/offset`), with analytically recomputed
normals/TBN, feeding the existing PBR lighting. Real silhouettes, contained blast radius.

## Approach (why VS displacement here)

At a fixed, near-constant preview camera distance, **uniform subdivision is the correct amount of
geometry** — screen-space adaptivity buys nothing. The vertices genuinely exist, so the displaced sphere
integrates with shadows and (later) RT for free. This is the simple, correct baseline; the general
in-scene system (Phase B) is where adaptivity and cost live.

## Touch points

- **`geometry::primitives`** — a densely-subdivided sphere/plane variant (or a subdivision parameter on
  the phase-1 generators) suitable for displacement.
- **`preview.slang` / vertex path** — sample the height texture in the vertex shader; displace along the
  normal by `height_scale`; **recompute the normal analytically** (finite-difference height at u±ε,
  v±ε → cross the tangents), re-orthonormalize the TBN (extends the existing normal-map/TBN code in
  `lighting.slang`), then shade.
- **`thumbnail_render.rs` + `furnish_preview_scene`** — use the displaced sphere for height/displacement
  subjects.
- **Retire preview POM** — the preview path no longer uses `parallaxUv`; delete that branch from the
  preview shader (in-scene POM stays until Phase B retires it there).

## Verification

- A height map on the preview sphere shows a **deformed silhouette** at grazing angle (side-by-side vs
  the old smooth-outline POM).
- Lighting looks correct (analytic normals), not flat/wrong.
- `just engine` + shader compile clean.

## Risks (all small)

- **UV-sphere pole/wrap seams** — weld at generation time or use an icosphere (continuous
  parameterization). Decide with the phase-1 sphere topology choice.
- **Height mip aliasing** — clamp/filter the height sample.
- **`height_scale` units** — keep consistent with the eventual in-scene system (open question #2 in the
  README) so a `.smat` looks the same on the preview and in-scene.
