# Phase D2 — import routing + a sane default, and one mode across every preview surface

**Status:** IMPLEMENTED (engine + editor compile; import routing + role tests green — full GPU e2e
deferred to the test rehaul). A `detect_height_mode(filename)` (`scan.rs`) routes by provider label:
a **Displacement**/`_disp` map → `HeightMode::Displacement` (library intent + the three.js default), a
**bump** map → `HeightMode::Bump`, any other height map → `HeightMode::Parallax`. `MaterialMap` carries
the detected mode; `bake_material_container` sets `material.height_mode` from the height map, so an
ambientCG **Rock 063** import lands in Displacement mode — fixing the swim. Preview surfaces are now
unified on the material's own mode: `enter_material_preview` puts the material by id on the dense
`PREVIEW_DISPLACE_SPHERE_MESH_ID` (global displacement defaults on, so it displaces for real), and the
standalone-texture Height preview sets Displacement too; the small offscreen thumbnail keeps its bump
(indistinguishable at 128px). **Note:** real displacement still needs tessellated geometry — the dense
preview sphere shows it, but a low-poly *scene* mesh falls back to the bump/parallax look until the B2
adaptive-tessellation layer lands (surfaced by the editor's Height mode dropdown).
**Scope:** `saffron-assets` (`scan.rs` role detection, `import.rs` `bake_material_container`),
`saffron-control` (`enter_material_preview`, the ephemeral texture-preview material,
`thumbnail_render.rs`), the storefront connector (map-label provenance)
**Depends on:** **D1** (the `HeightMode` enum + a Bump fallback to route into). Closes the reported
**Rock 063 swimming** defect.

## The defect (grounded)

Importing an ambientCG material (Rock 063: Color / NormalGL / Roughness / AO / **Displacement**) wires
the Displacement map into `height_texture` but leaves the mode at the default — so it renders through
the 24-step fragment POM (`FEATURE_HEIGHT`, `parallaxUv` in `lighting.slang:540`), whose offset
`(vt.xy / max(|vt.z|,0.1)) * scale` blows up as `vt.z→0`. The surface **swims/melts at grazing
angles**; ambientCG's own reference render is clean because it uses **real displacement**. A library
*Displacement* map is a large-amplitude heightfield authored for geometry, not the subtle depth POM
assumes — routing it to POM is guaranteed to swim.

**Inconsistency to also fix (verified):** the same map is treated three different ways across our
preview surfaces —
- interactive standalone-**texture** preview forces real displacement
  (`commands_asset.rs:3059`, `TextureRole::Height → displacement=true, height_scale=0.08`);
- the **thumbnail** render uses `SubmeshMaterial::defaults()` → POM/bump (`thumbnail.rs:469`,
  `draw_list.rs:96`);
- the interactive **material** preview + the **imported material** use the authored default → POM.

So a lone height *texture* looks right while the *material* carrying it swims. D2 makes all four agree.

## The one design decision (confirm before building)

What should an imported **Displacement**-named map default to? The 2026 survey (README) shows a real
tension with "work like the major game engines":

- **Match the editor engines** → default **Parallax/Bump, displacement opt-in** (Unreal/Unity/Godot/
  Blender all default a height map to off/bump/POM).
- **Honor the asset's intent** → default **Displacement** (ambientCG/Poly Haven/Megascans author these
  *for* real mesh displacement; **three.js** — our closest architectural peer, a from-scratch renderer —
  auto-displaces a `displacementMap`). This is what makes Rock 063 match its reference.

**Recommendation:** route by **provider map label**, since the importer already knows it — a
provider-labeled **Displacement** map → `Displacement` mode; a **Height / Bump / Parallax** map →
`Parallax`; anything else → `Bump`. This fixes Rock 063, honors library intent + the three.js precedent,
and D1's per-material dropdown still lets the user override. It is a deliberate divergence from the
editor engines' "opt-in" default — flagged as such.

## Approach

1. **Carry the provider intent to the importer.** `detect_material_role` (`scan.rs`) currently
   collapses both "Displacement" and "Height" filenames to a single `height` role. Split the signal so
   the baker can pick a mode: either a distinct `displacement` vs `height` role, or a mode hint derived
   from the source slot name (the storefront connector knows each map's label). 16/32-bit source → a
   low-confidence tiebreaker toward Displacement (Poly Haven ships 16-bit).
2. **Route in the baker.** `bake_material_container` (`import.rs`) sets `height_mode` from that signal
   instead of leaving the default, and sets a sensible **displacement amplitude** for the Displacement
   case (not the 0.05 parallax depth — align with the preview's 0.08 world-space amplitude and
   open-question #2's "same on preview and in-scene" rule).
3. **Unify the preview/thumbnail paths.** With the material now authored in the right mode,
   `enter_material_preview` (`commands_asset.rs:2985`, already references the material **by id** on the
   dense `PREVIEW_DISPLACE_SPHERE_MESH_ID`) shows a true displaced silhouette for free. Fix the
   **thumbnail** builder (`thumbnail.rs:469`) and the ephemeral **texture**-preview material
   (`commands_asset.rs:3030`) to derive their mode the same way, so tile ↔ interactive ↔ material ↔
   ambientCG reference all agree — one rule, no per-surface special-casing.

## Verification

- Import Rock 063 → a material in **Displacement** mode with a sensible amplitude; its preview sphere
  shows a **deformed silhouette matching ambientCG's reference**, not swimming; the thumbnail tile
  matches the interactive preview.
- A **Height**-named import → **Parallax**; a set with neither → **Bump**.
- End-to-end via `just run`: Asset Store import of Rock 063 (and a Poly Haven material) previews
  correctly, opens in the asset editor with the mode dropdown reflecting the chosen mode.

## Risks

- **Real displacement needs geometry.** The preview sphere is dense (fine); an imported material on a
  **low-poly scene mesh** shows little silhouette until the in-scene adaptive-tessellation layer exists
  (B2 deferred / C2) — so surface the mode in the inspector and let Bump/Parallax be the scene-mesh
  fallback. Do not silently promise displacement the geometry can't show.
- **Provider-label provenance** must be threaded from the connector through the import; a raw
  drag-dropped folder without provider metadata falls back to filename heuristics (already how
  `detect_material_role` works).
