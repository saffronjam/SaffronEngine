# Sky atmosphere, clouds & time-of-day

**Status:** PENDING IDEA

> Inspiration backlog — not yet implementable as written. Needs a codebase pass (the aerial-perspective
> froxel, a "sun" directional-light tag, and sky-light re-bake into the existing IBL).

> **Split out (graduated to a planset):** height + froxel + local fog → [`../volumetric/`](../volumetric/README.md). This file
> covers the **sky atmosphere**, **aerial perspective**, **volumetric clouds**, and the **time-of-day**
> driver. A physically based sky already exists (the Hillaire LUT chain drives the env cube + IBL); what
> is missing here is dynamism + aerial perspective + clouds.

The biggest "AAA look" jump per unit of effort. Making the sky dynamic gives **time-of-day nearly free**,
and applying the atmosphere to distant geometry (aerial perspective) is what ties the scene to its sky.

## What it is

A dynamic physically based sky + sun, aerial perspective on distant geometry, optional volumetric clouds,
and a time-of-day driver that animates all of it.

- **UE5:** Sky Atmosphere + Volumetric Clouds + a day-night cycle driving the sun.
- **Unity:** Physically Based Sky + HDRP Volumetric Clouds.

## Core technique

**Sky (Hillaire 2020, already built):** a **Transmittance LUT** + **Multiple-Scattering LUT** + per-frame
**Sky-View LUT** (Rayleigh air + Mie haze + ozone). This chain exists today (`ibl.rs`
`EnvSource::Atmosphere`, the `atmos_*` shaders) and feeds the env cube + IBL. Because the LUTs rebuild
cheaply, animating the sun = **dynamic time-of-day for free**.

**Aerial perspective (the gap):** fill an **aerial-perspective froxel** volume so distant geometry inherits
atmospheric scattering — today the atmosphere only paints the sky backdrop, never the scene. This is the
haze that makes far objects read as far.

**Volumetric clouds (Nubis-derived):** Perlin–Worley base shape + Worley erosion, ray-marched with a Beer
shadow map and ~16-frame temporal reconstruction.

## Build size

- The **4-LUT sky is done**; the remaining sky work is **dynamism + a sun tag + sky-light re-bake**.
- **S** aerial-perspective froxel (shares the [`volumetric`](../volumetric/README.md) froxel infrastructure).
- **S** time-of-day driver (controller + `sa` scrub command + sky-light re-capture) — near-free once the
  sky is dynamic.
- **L–XL** volumetric clouds (gated on the sky atmosphere).

## Dependencies (do these first)

- A **"sun" directional-light tag** + a **sky-light re-bake** from the LUT into the existing IBL/ReSTIR
  environment so GI follows the time of day. Voxel-GI / DDGI reconvergence already exists.
- **Aerial perspective** shares the froxel infrastructure with the [`volumetric`](../volumetric/README.md) planset — build
  the froxel resource once.
- *Clouds:* transient 3D resources help (a known render-graph gap), not required.

## What we reuse / what's missing

**Reuse:** the existing Hillaire sky LUT chain + `EnvSource::Atmosphere`, compute (the LUT sweet spot),
bindless, motion vectors + TAA (cloud/aerial reprojection), and the IBL/ReSTIR environment that already
consumes an env cube.

**Missing:** the aerial-perspective froxel, the sun/sky-light tagging + a re-bake hook, a 1D curve-editor
for TOD ramps (shared enabler), and cloud density authoring (a "volume" material-graph domain).

## Editor UX / authoring

The atmosphere is already scriptable (`set-atmosphere` / `set-environment`). Time-of-day adds a **sun
angle / time scrubber** in the environment panel that re-bakes the sky-light live; clouds add coverage/
density controls. Aerial perspective is automatic once on. Cross-links to the [`volumetric`](../volumetric/README.md) planset
for ground haze.

## Notes & references

- Hillaire, "A Scalable and Production Ready Sky and Atmosphere Rendering Technique" (2020) — the built
  LUT method.
- Schneider & Vos, "The Real-time Volumetric Cloudscapes of Horizon Zero Dawn" (Nubis) — clouds.
