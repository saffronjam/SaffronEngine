# Surface detail & screen effects

**Status:** PENDING IDEA

> Inspiration backlog — not yet implementable as written. Needs a codebase pass (a forward decal pass and
> a "Decal" domain for the material node-graph, plus the lens-artifact post passes).

> **Split out:** bloom & color grading graduated to the [`post-processing`](../post-processing/README.md)
> planset; snow/wetness/precipitation → [weather-precipitation](weather-precipitation.md). This file now
> covers **decals** + **lens/camera artifacts** (chromatic aberration, vignette, film grain, lens flare).

Small, high-polish items with no new primitives — the fastest way to make the image read as "finished."
Lens artifacts are genuine gaps; decals reuse the material node-graph + Jolt raycast.

> **Architecture note:** the renderer is **forward+**, so there is no G-buffer — **DBuffer decals are
> not available**. Decals must be a bespoke forward screen-space or clipped-mesh pass. SSR is
> **deprioritized/skipped** — it is redundant with the existing ReSTIR + RT reflections + SSGI; only
> worth it as a low-end no-RT fallback.

## What it is

Runtime decals (bullet holes, blood, scorch) and lens/camera post artifacts.

- **UE5:** deferred/mesh decals + Post Process Volume lens effects.
- **Unity:** the Decal Projector + post-processing lens effects.

## Core technique

- **Decals:** project a texture onto surfaces — either unproject from depth in a forward screen-space
  pass, or clip a decal mesh to the receiver. Placement uses Jolt raycast (hit point + normal).
- **Lens/camera FX:** **chromatic aberration**, **vignette**, and **film grain** — cheap full-screen
  passes *after* tonemap.
- **Lens-flare ghosts:** sample bright spots and splat scaled/offset ghosts along the optical axis.

## Build size

- **S–M** lens/camera FX (CA, vignette, film grain) — high polish per effort; genuine gaps.
- **M** runtime projected decals (forward screen-space) / **L** clipped mesh decals.
- **M** screen-space lens-flare ghosts.

## Dependencies (do these first)

- **Lens FX and decals need nothing new** — pure render-graph passes + node types.
- *Decals on moving entities* want **scene-graph parenting**.

## What we reuse / what's missing

**Reuse:** the render graph (post passes), tonemap (lens passes slot after it), the material node-graph (a
"Decal" domain), Jolt raycast (decal placement), and TAA (keeps screen-space effects stable).

**Missing:** the lens-artifact passes themselves and a forward decal pass (forward+ forbids DBuffer).

## Editor UX / authoring

Lens artifacts are camera/post properties in the same panel as the
[post-processing](../post-processing/README.md) bloom & grade controls, driven live over the control
plane. Decals are placed by raycast — a
paint/stamp tool that emits placement commands, with the decal texture chosen from the asset catalog.

## Notes & references

- "Next Generation Post Processing in Call of Duty: Advanced Warfare" (Jimenez) — lens FX references.
- UE5/Unity decal docs — note the forward+ caveat above (no DBuffer for us).
