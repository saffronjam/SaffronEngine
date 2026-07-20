# Phase 10 — Shared wind, structured deformation, interaction, and phenology

**Status:** NOT STARTED

**Depends on:** Phases 2, 5, and 8

This phase extends the shipped global wind settings into one cross-system sampled wind field and adds
vegetation as a structured deformation provider. Plant assets own physical response; Environment owns
the driving field/calendar. Compute produces current/previous deformation and swept bounds once for
depth, shading, motion, shadows, GI, and ray tracing.

## Shared wind ownership

- [ ] Add a cross-cutting `saffron-wind` module/crate or equivalent environment service depending on
  core/geometry/spatial, not on vegetation, rendering, cloth, particles, or physics.
- [ ] Extend `SceneEnvironment::wind` beyond orientation/speed/gust with deterministic multiscale
  spectral/turbulence parameters, height/terrain response, gust fronts, and time sampling while
  preserving it as the one global source.
- [ ] Add placeable local wind-source components for directional/point/vortex/wake/volume influence.
  Edit them in Inspector/viewport; do not add plant wind controls to Environment.
- [ ] Compile global and local sources into a clipmapped/vector field sampled identically by clouds,
  fog, future cloth/particles/weather, physics queries, and vegetation.
- [ ] Use a monotonic simulation clock for evolution. Calendar affects seasonal signals, not wind
  integration or biological age.

## Plant response and deformation provider

`.splant` supplies hierarchy/spines, rest pivots/orientations, stiffness, damping, drag, bend limits,
mass, leaf/flutter modes, phase variation, break/damage response, and per-cluster influence/swept
bounds.

- [ ] Generalize the Phase-7 skin/morph/displacement seam into one deformation-provider contract
  that outputs current/previous transforms/vertices, cluster bounds, and optional BLAS inputs.
- [ ] Evaluate tree/shrub branch modes or bone chains in compute from the shared wind field and local
  impulses; preserve hierarchy length/limits and stable per-plant phase.
- [ ] Evaluate grasses/blades as analytic curve deformation through the same output contract.
- [ ] Deform assembly parts without expanding authored structure; compute tight node/cluster swept
  bounds rather than inflating whole-tree bounds.
- [ ] Share one result with depth, main, motion, selection, fixed shadows, aggregate voxels, and later
  VSM/RT. No shader independently re-evaluates wind.
- [ ] Reduce far deformation state by screen-space error/modal aggregation. Never abruptly stop wind
  or snap to bind pose at distance.

## World interaction field

- [ ] Add world-space tiled/clipmapped displacement and velocity fields with generation-safe
  residency and damped-oscillator recovery.
- [ ] Accept swept capsule/sphere/volume impulses from editor previews, characters, animals, vehicles,
  explosions, and wind gusts through one emitter API.
- [ ] Sample the field inside the same vegetation deformation provider; no trample material/shader.
- [ ] Keep cosmetic predicted bend separate from persistent crushed/cleared/damaged disturbance
  mutations. Persistent masks flow through the Phase-2 reducer.
- [ ] Aggregate voxel nodes animate occupancy/normal distributions consistently with their simplified
  deformation state.

## Phenology and lifecycle rendering

- [ ] Derive current phenology from existing date/latitude/time plus plant intrinsic curves and
  persistent lifecycle/health/moisture state.
- [ ] Blend/crossfade cooked growth, seasonal, flower/fruit, damaged, dead, harvested, wet, and burn
  phenotype variants through stable hierarchy/coverage transitions.
- [ ] Keep biological age monotonic and independent of editor calendar scrubbing.
- [ ] Rendering reads typed lifecycle state; it never infers lifecycle from an active mesh.
- [ ] Leaf-density changes, fruit/flower parts, and snow/wetness hooks modify the same assembly/
  aggregate hierarchy and material moments rather than spawning a second seasonal renderer.

## Temporal and diagnostic requirements

- [ ] Motion vectors use current and previous deformed positions from the same provider.
- [ ] Camera cuts, interaction-field resets, source edits, phenotype jumps, and page changes emit exact
  TAA/history invalidation/reactive coverage.
- [ ] Debug wind vectors/spectra, local source influence, branch modes, stiffness, current/previous
  bounds, interaction displacement/velocity/recovery, and phenotype weights in `sa` and editor.

## Acceptance

- [ ] Wind phase/frequency/amplitude scale plausibly with plant structure and are stable across
  residency, origin rebasing, and executor choice.
- [ ] Depth/main/motion/current-shadow/selection deformation agrees vertex-for-vertex or by the defined
  aggregate error; no TAA ghost trail follows gusts or phenotype transitions.
- [ ] Character/vehicle emitter recovery is damped and deterministic; persistent damage survives
  unload/reload while cosmetic bend does not pollute saves.
- [ ] Distant vegetation continues moving within the appearance-error bound.
- [ ] Tight swept bounds prevent HZB false occlusion and are available to VSM/RT phases.
- [ ] Clouds/fog and vegetation sample the same global/local wind field and time.
- [ ] Standard gate, platform validation/visual tests, and wind/phenology docs are green.

## NO-LEGACY gate

Delete any plant WPO/wind material node or independent trample implementation if one exists by this
phase. Environment wind, local source components, the sampled field, and the deformation provider are
the only path.

