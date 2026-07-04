# Phase A — displaced preview via pre-subdivided base + VS displacement

**Status:** IMPLEMENTED. A height-map material now moves **real vertices** on a densely-subdivided
preview sphere — a true deformed silhouette in the interactive preview, not the smooth-outline POM.
Pieces: `saffron_geometry::preview_displacement_sphere()` (192×288 UV sphere) seeded under the reserved
`PREVIEW_DISPLACE_SPHERE_MESH_ID` (cache-first, not spawnable); a `FEATURE_DISPLACE` bit + a
`displacement: bool` on `MaterialAsset` / `SubmeshMaterial` (serialized, carried through
`build_submesh_material` → `resolve_material`, mutually exclusive with the parallax `FEATURE_HEIGHT`);
**VS displacement** in the übershader — `applyVertexDisplacement` in `lighting.slang` samples the height
map in `transformVertex`/`transformVertexSkinned` and offsets the world position along the world normal
by `height_scale`; the `mesh.slang` fragment derives the fine shading normal from the height gradient (a
bump) so lighting matches the displaced microsurface. The interactive texture/material previews use the
dense sphere; a standalone height texture enables displacement. This **completes
`texture-material-previews/phase-5`** (the height preview it left wired-but-POM). **Scope note:** the VS
path lives in the übershader (gated by `FEATURE_DISPLACE`), so the *interactive* preview — the primary
inspection surface — shows the real silhouette; the small offscreen **thumbnail keeps its height→normal
bump** (indistinguishable at 128px and avoids a dense-sphere re-render per thumbnail). In-scene POM
(`FEATURE_HEIGHT`) is untouched, retired for general meshes by Phase B2. **Verified:** shader compile +
`cargo build --workspace` + `clippy --workspace -D warnings` + `fmt --check` + geometry/material unit
tests green. **Visual** (true silhouette at grazing angle) needs a GPU+eyes run — the mechanism is
sound but I can't see the frame here.

> **Superseded by B2 (NO-LEGACY):** the übershader *vertex-shader* displacement this phase added has been
> **retired** — Phase B2's `displace` compute pre-pass is now the one displacement mechanism (it bakes the
> height offset into the shared deformed buffer, consistent across every pass, including the point-shadow
> cubes the VS path missed). Everything else Phase A introduced stays and is still how the preview works:
> the dense `preview_displacement_sphere`, the reserved id, the `displacement` material flag +
> `FEATURE_DISPLACE` bit + the `mesh.slang` height-gradient shading normal, and the preview wiring. The
> interactive preview sphere is a scene instance with a displacement material, so B2's compute pre-pass
> displaces it automatically — same visual, one code path.
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
