# Per-type texture thumbnails + preview.slang slot completion

**Status:** NOT STARTED
**Scope:** `saffron-assets`, `saffron-rendering` (`preview.slang`)
**Depends on:** phase-1 (role known). Sphere thumbnail path is otherwise unblocked (uses the existing
offscreen `make_preview_sphere`).

## Goal

A standalone texture's grid thumbnail renders it on the studio sphere in its correct role (not a flat
swatch), by synthesizing an ephemeral single-slot material. The thumbnail shader gains the slots it
lacks.

## Touch points

- **`assets/src/thumbnail.rs`** — `generate_thumbnail` currently sends a standalone `Texture` down the
  flat-downscale path. Branch on `role`: synthesize an ephemeral `SubmeshMaterial` (`draw_list.rs`,
  every slot present) with the texture in the role's slot and neutral factors elsewhere ("neutral
  albedo" = mid-grey), then `render_material_preview`. `Unknown`/unroutable → keep the flat downscale.
- **`engine/assets/shaders/preview.slang`** — today reads albedo / MR / normal only. Add: **occlusion**
  sampling (AO, ORM), **emissive** + dark background, **height** (via `parallaxUv` for the *thumbnail*;
  the interactive tab gets real VS-displacement in phase-5), **alpha discard** + checkerboard backdrop.
  Add a **normal flip-Y** uniform (GL/DX). Keep it minimal — do **not** re-implement the full lighting
  path; route anything needing true IBL (metallic especially) to the interactive tab.

## Verification

- Grid thumbnails: albedo lit, normal reads as bumps under the key light, roughness shows tight-vs-
  smeared highlight, AO darkens cavities, emissive glows on dark, opacity shows cutout over checker.
- HDR/HDRI still uses the flat tonemapped lat-long thumbnail (cheap); its rich preview is phase-4.
- `just engine` + shader compile clean; existing thumbnail tests pass.

## Notes

- Metallic on the 2-light studio thumbnail stays slightly misleading (no IBL) — accepted for the small
  tile; it reads correctly on the interactive IBL tab (phase-3/4). This is the deliberate minimal-shader
  line.
