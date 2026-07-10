# Pending Ideas

**Status:** PENDING IDEA (this whole folder)

An inspiration backlog of feature families worth building next, gathered from how Unreal Engine 5 and
Unity approach them and weighed against what Saffron Anima already has. These are **not yet
implementable as written** — each is more than a `todo.md` line but less than a real plan. Turning one
into work needs a codebase pass to ground it in current symbols/files, at which point it graduates to
its own `plans/<feature>/` folder with numbered phase files.

One markdown file per distinct idea. Each lists what it is, how the big engines do it, the core
technique, a rough build size, the **dependencies that must land first** (engine gaps *and* other
pending ideas), and what we reuse vs. what's missing.

## The two strategic facts that shape this list

1. **Jolt already contains whole subsystems** other engines hand-wrote — vehicles, cloth/soft-body,
   heightfield collision, breakable constraints. For those, the hard solver is already vendored; the
   work is the cxx FFI surface, ECS components, and editor UX.
2. **The compute + render-graph + node-graph→Slang stack** makes GPU-authored families (particles,
   volumetrics, water) cheap to host — the render graph already derives barriers, and the material
   node editor already codegens Slang.

## Cross-cutting enablers (build to unlock breadth)

A handful of primitives gate a disproportionate share of the catalog. Sequence these deliberately —
they are the cheapest way to unblock the most features.

| Enabler | Unlocks | Notes |
|---------|---------|-------|
| **GPU particle/sim runtime** (persistent buffers + indirect-draw args + GPU sort) | smoke/fire, FLIP liquids, weather, destruction dust, water splashes | also lays the indirect-args groundwork the GPU-driven-culling gap needs — build once, retire two gaps. See [gpu-particle-vfx](gpu-particle-vfx.md). |
| **Heightfield terrain core** (16-bit height asset + quadtree LOD) | terrain collision, splat materials, sculpt brushes, foliage scatter, water shorelines | See [heightfield-terrain](heightfield-terrain.md). |
| **Scene-graph parenting** (already a known gap) | prefabs, sequencer attach tracks, destruction clusters, networked hierarchies | cheapest high-fanout enabler — small to build, blocks a lot. |
| **cxx-FFI vendoring pattern** (proven by Jolt) | recastnavigation → navmesh → AI | template reused wholesale. |
| **Stable entity GUIDs + partial registry (de)serialization** | save/load, cell streaming, network replication | |
| **GPU-FFT utility** | FFT ocean, FFT/convolution bloom | |
| **1D curve-editor widget** | vehicle torque/friction curves, time-of-day ramps, post-FX tuning | |

## Catalog

| Idea | Build size | Key dependency | One-liner |
|------|-----------|----------------|-----------|
| [wheeled-vehicles](wheeled-vehicles.md) | M | none | Jolt already ships the entire vehicle solver — top ROI. |
| [cloth-and-soft-body](cloth-and-soft-body.md) | S–M | none | Jolt soft bodies + existing compute-skinning ingestion. |
| [gpu-particle-vfx](gpu-particle-vfx.md) | L | indirect-args/persistent buffers | keystone enabler for all VFX. |
| [smoke-fire-and-fluids](smoke-fire-and-fluids.md) | M–XL | gpu-particle-vfx (FLIP) | Eulerian gas solver + volumetric render. |
| [sky-atmosphere-and-volumetrics](sky-atmosphere-and-volumetrics.md) | S–XL | none (clouds want sky first) | dynamic sky + aerial perspective + clouds + time-of-day (fog → volumetric-fog). |
| [volumetric-fog](volumetric-fog.md) | S–M | none (clustered lights/shadows present) | height + froxel fog + god-rays; no fog of any kind today. |
| [heightfield-terrain](heightfield-terrain.md) | L | none | enabler for foliage/water/sculpting. |
| [foliage-and-vegetation](foliage-and-vegetation.md) | M–XL | heightfield-terrain (canonical use) | painting, wind, interactive bending, PCG. |
| [wind](wind.md) | S–M | none | a shared wind field foliage/cloth/particles all read. |
| [water-and-ocean](water-and-ocean.md) | S–L | gpu-fft (ocean), terrain (rivers) | Gerstner buoyancy is the cheap gameplay win. |
| [destruction-and-fracture](destruction-and-fracture.md) | L–XL | parenting, gpu-particle-vfx (dust) | Voronoi fracture + strain runtime on Jolt. |
| [procedural-cameras-and-cinematics](procedural-cameras-and-cinematics.md) | S–XL | parenting (sequencer) | vcam/brain, collision, shake, cinematic DoF. |
| [ai-navigation-and-behavior](ai-navigation-and-behavior.md) | S–XL | cxx vendoring (navmesh) | perception + behavior trees are the cheap start. |
| [audio-system](audio-system.md) | M–XL | none (greenfield crate) | spatial audio, occlusion, reverb, music. |
| [surface-detail-and-screen-fx](surface-detail-and-screen-fx.md) | S–L | none | decals + lens artifacts (CA/vignette/grain/lens-flare). |
| [weather-precipitation](weather-precipitation.md) | S–XL | gpu-particle-vfx + wind | snow/rain particles + snow/wetness accumulation. |
| [gameplay-framework](gameplay-framework.md) | S–XL | parenting + GUIDs (prefabs/save) | input mapping, tags, prefabs, save/load, GAS. |
| [networking-multiplayer](networking-multiplayer.md) | L–XL | GUIDs + parenting | rollback is the determinism-differentiated option. |
| [large-worlds-streaming](large-worlds-streaming.md) | L–XL | GUIDs + parenting | defer unless target worlds demand it. |

**Graduated to a planset:** bloom + color grading → [`../post-processing/`](../post-processing/README.md) — energy-conserving pre-tonemap bloom + a scene-linear grade folded into the tonemap pass + a creative LUT slot.

## Suggested tiers

- **Tier 0 — self-contained quick wins:** wheeled-vehicles, cloth-and-soft-body, **lens FX** (the post
  look; bloom + color grading are now the [`post-processing`](../post-processing/README.md) planset),
  **height fog** + time-of-day, the **wind field**, decals, AI perception +
  gameplay tags + input mapping, **scene-graph parenting** (the gap).
- **Tier 1 — foundational enablers:** GPU particle runtime, heightfield terrain + collision, GPU-FFT
  utility, curve-editor widget, procedural camera, navmesh + pathfinding + behavior trees.
- **Tier 2 — built on Tier 1:** froxel volumetric fog + aerial perspective, smoke/fire, FFT ocean + water
  shading + buoyancy, foliage (samples the wind field), terrain sculpting, destruction, prefabs +
  save/load, cinematic Sequencer, audio engine.
- **Tier 3 — large programs (defer):** volumetric clouds, FLIP liquids + weather precipitation,
  SpeedTree/PCG/virtual heightfield (need GPU culling), full GAS, networking program, world streaming.

## Conventions

These follow `AGENTS.md`. When an idea graduates to a real plan it becomes `plans/<feature>/` with a
`README.md` + numbered `phase-N-*.md` files and a `NOT STARTED`/`IN PROGRESS`/`COMPLETED` status, per
the `plans/` rules. Delete a pending-idea file once its plan folder exists (no duplication).
