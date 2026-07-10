# Volumetric & height fog

**Status:** PENDING IDEA

> Inspiration backlog — not yet implementable as written. Needs a codebase pass (a froxel 3D resource in
> the render graph — a known transient-3D gap — density/injection/integrate passes, a `Fog`/`FogVolume`
> scene component + DTO, and `set-fog` / fog-volume control commands).

There is **no fog of any kind** today — not distance fog, not height fog, not volumetrics. The Hillaire
sky ([sky-atmosphere-and-volumetrics](sky-atmosphere-and-volumetrics.md)) is a sky/IBL model that never
touches scene geometry as haze. Fog is the missing "atmosphere in the space" — on the reference scene it
is the cold haze filling the plaza *and* the red light visibly scattering through it toward the camera.

## What it is

Participating-media fog: a cheap analytic distance/height fog, a physically based froxel volume that
scatters light (god-rays / light-shafts), and artist-placed local fog volumes.

- **UE5:** Exponential Height Fog + Volumetric Fog (a per-light volumetric flag) + light shafts.
- **Unity:** HDRP Volumetric Fog + local Density Volumes; fog in the Volume system.

## Core technique

- **Analytic exponential height fog** — a per-pixel term over depth with distance + height falloff and an
  inscattering color (which can be tinted toward the neon red). Far cheaper subset — do this first.
- **Froxel volumetric fog** — a camera-frustum 3D grid: **density injection** → **per-froxel light
  scatter** (Henyey–Greenstein phase, reusing the clustered light list and *every* shadow map, so
  shadowed volumes carve god-rays) → a **ray-march integration** scan → **temporal reprojection** (reuse
  motion vectors + TAA history). This is where light shafts fall out for free.
- **Local fog volumes** — box/sphere entities that add density (the fog bank pooling at the building
  base); want scene-graph parenting to ride moving entities.
- **Aerial-perspective coherence** — when the sky atmosphere is present, share its inscattering so fog
  and sky agree at distance (the aerial-perspective froxel is itself a sky-side gap; cross-link).

## Build size

- **S** analytic height fog.
- **M** froxel volumetrics (inject + integrate + reproject).
- **M** local fog volumes (box/sphere density).
- **S** light-shaft/god-ray tuning (falls out of froxel + shadows).

## Dependencies (do these first)

- **Nothing hard-required** for height fog.
- **Froxel fog** reuses clustered lights + every shadow type + TAA — all present — but wants a **transient
  3D froxel image** (a known render-graph resource gap).
- **Local fog volumes** want **scene-graph parenting** (present — the `Relationship` component).
- **Coherent aerial perspective** wants the **sky atmosphere** (present as sky/IBL; the aerial-perspective
  froxel is the gap noted in the sky file).

## What we reuse / what's missing

**Reuse:** the render graph (compute inject + integrate passes), the clustered **froxel light cull** that
already exists (`light_cull.slang` — the same grid shape), every shadow type (light injection), motion
vectors + TAA (volumetric reprojection), and the environment/atmosphere params.

**Missing:** the density/injection/integration passes + a transient 3D froxel resource, a `Fog` scene
component + a `FogVolume` entity + their DTOs, and `set-fog` / fog-volume control commands.

## Editor UX / authoring

Scene-wide fog lives in the **environment panel** (density, height falloff, inscatter color, scattering
anisotropy) via a `set-fog` control command + `sa`, sitting beside `set-environment`/`set-atmosphere`.
Local fog volumes are **placeable entities** (`add-entity fog-volume`) with a box/sphere gizmo, authored
just like lights. Everything scriptable from the shell so the look is reproducible.

## Notes & references

- Wronski, "Volumetric Fog" (Assassin's Creed 4) — the froxel injection/integration approach.
- Hillaire, "Physically Based and Unified Volumetric Rendering" (Frostbite) — froxel media + aerial
  perspective coherence.
- UE5 Exponential Height Fog / Volumetric Fog docs.
