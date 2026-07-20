# Weather & precipitation (snow, rain)

**Status:** PENDING IDEA

> Inspiration backlog — not yet implementable as written. Needs a codebase pass (rides the
> [gpu-particle-vfx](gpu-particle-vfx.md) keystone + the shared wind field scheduled in
> [`foliage-veg` Phase 10](../foliage-veg/phase-10-wind-deformation-phenology.md); a weather-state
> component/uniform + `set-weather` command; snow/wetness material nodes on the node-graph).

The falling, wind-blown snow that gives the reference scene its storm mood. This has two halves: the
**particles in the air** (which hang off the particle keystone and the wind field) and the **surface
response** on the ground and building (accumulation/wetness, which is pure material work and needs no
particles).

## What it is

A weather layer: precipitation particles (snow, rain), their collision splashes, and the surface response
(snow accumulation, wetness) as the world reacts.

- **UE5:** Niagara-driven rain/snow systems + material-layer wetness/snow accumulation; Ultra-Dynamic-
  Sky-style weather assets.
- **Unity:** VFX-Graph precipitation + Shader-Graph wetness/snow layers.

## Core technique

- **Precipitation particles** — GPU particles (from the keystone) spawned in a **camera-following spawn
  box** (particles only exist where seen), advected by gravity + the **wind field** + curl-noise
  turbulence, rendered as **stretched streak sprites** (rain) or **soft flake sprites** (snow). Because
  they inherit the wind vector, snow blows sideways — the "windy snow" of the reference.
- **Collision splashes/settle** — depth-buffer or RT/TLAS collision spawns splash (rain) or settle (snow)
  sub-emitters where flakes land.
- **Surface response (no particles)** — a global **weather uniform** drives material node types: a **snow
  layer** blended by world-up normal + a height/cavity mask, and **wetness** that darkens albedo, lowers
  roughness, and adds a thin specular sheen. Cheapest weather payoff; ships before the particles.

## Build size

- **S** weather uniform + snow/wetness material nodes (no particles) — the cheap half.
- **M** snow/rain particle emitters (on the keystone).
- **M** splash/settle sub-emitters (RT/depth collision).
- **L** a weather controller / state machine (optionally driven by time-of-day).

## Dependencies (do these first)

- **[gpu-particle-vfx](gpu-particle-vfx.md)** — the keystone — for the falling particles.
- **[`foliage-veg` Phase 10](../foliage-veg/phase-10-wind-deformation-phenology.md)** — the one
  shared wind field used for wind-driven advection (sideways snow).
- **Accumulation/wetness nodes need nothing new** — they ride the material node-graph.
- **Collision splashes** reuse RT/TLAS or scene depth (present).

## What we reuse / what's missing

**Reuse:** the (future) GPU particle runtime, the material node-graph (React Flow → Slang) for snow/
wetness nodes, the wind field, RT/TLAS + depth collision (splashes), and TAA (stable sprites).

**Missing:** the particle system itself (keystone) and the wind field (both separate pending ideas); a
weather-state component/uniform + a `set-weather` control command; and the snow/wetness material nodes.

## Editor UX / authoring

A **weather section** in the environment/scene panel — precipitation type, intensity, wind coupling,
accumulation amount — over a `set-weather` command + `sa`. Snow and wetness surface as **material-graph
nodes** so any surface opts in. If time-of-day exists, weather state can be keyed along it.

## Notes & references

- *Ghost of Tsushima* GDC talk — wind-coupled weather + snow.
- UE Niagara weather systems + Ultra Dynamic Sky; God of War snow-deformation / footprint talks
  (accumulation); Frostbite wet-surface references.
