# Wind field

**Status:** PENDING IDEA

> Inspiration backlog — not yet implementable as written. Needs a codebase pass (a wind uniform +
> optional low-res 3D wind texture, a `Wind` env field / `WindSource` entity + `set-wind` command, a Wind
> material node, and a force hook into Jolt for cloth/bone-physics).

A small, cross-cutting enabler: one shared wind source that foliage, cloth, particles, snow, and hair all
read. Building it once makes "windy" possible everywhere. Today there is **no wind concept at all** — the
only `wind` matches in the tree are triangle winding-order comments, and physics exposes only a
`gravity_factor` with **no external-force API**, so nothing can be pushed by wind yet.

## What it is

A global wind vector with gusts and turbulence, plus optional local wind sources, sampled by every system
that has secondary motion.

- **UE5:** Wind Sources (Directional / Point) + a global wind; a `Wind` material node; Niagara wind
  forces; cloth wind.
- **Unity:** WindZone; Shader-Graph wind; VFX-Graph turbulence.

## Core technique

- **Global wind** — a direction + strength scene uniform, with **gusts** (time-varying magnitude from
  layered noise or a 1D gust curve).
- **Local wind sources** — directional / point / vortex entities with falloff, summed into the field;
  optionally **baked into a low-res 3D wind texture** the GPU samples cheaply (the UE model).
- **Turbulence** — curl-noise perturbation for natural swirl.
- **Consumers sample the field:** foliage vertex-wind nodes, cloth / soft-body external force, particle
  advection (snow/rain), hair.

## Build size

- **S** global wind vector + gust noise (a uniform).
- **M** local wind sources + 3D wind-texture bake.
- **S** curl-noise turbulence module.

## Dependencies (do these first)

- **Nothing to start** — a scene uniform.
- **Local sources** want **scene-graph parenting** (present) to attach to entities.
- Consumers ([foliage-and-vegetation](foliage-and-vegetation.md), [cloth-and-soft-body](cloth-and-soft-body.md),
  [weather-precipitation](weather-precipitation.md)) are separate ideas — wind is the shared field they read.

## What we reuse / what's missing

**Reuse:** the scene/environment settings + control plane, the material node-graph (a Wind sample node),
and — once they exist — foliage vertex wind, particle forces, and Jolt cloth/soft-body (wind as an applied
external force).

**Missing:** the wind field itself (uniform + optional 3D texture), a `Wind` env field / `WindSource`
entity + a `set-wind` command, a Wind material node, and the **external-force hook into Jolt** (cloth and
bone-physics take only gravity today — this adds the first external force).

## Editor UX / authoring

A **wind section** in the environment panel (direction, strength, gust, turbulence) via `set-wind` + `sa`;
local wind sources as **placeable entities** with a direction gizmo; a **Wind node** in the material
graph. One field, many consumers — so "turn up the wind" moves grass, cloth, and snow together.

## Notes & references

- UE5 Wind Sources / Niagara wind forces.
- *Ghost of Tsushima* wind system (GDC) — a game built around a single readable wind field.
- Bridson et al., curl-noise — divergence-free turbulence.
