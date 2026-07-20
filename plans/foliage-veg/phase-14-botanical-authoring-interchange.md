# Phase 14 — Native botanical authoring and interchange

**Status:** NOT STARTED

**Depends on:** Phases 2, 6, 9, and 10

This phase makes Anima capable of authoring coherent plant families rather than relying only on
imported static meshes. A procedural/manual botanical graph lives inside `.splant` and compiles to the
same normalized family, virtual geometry, material, wind, collision, phenotype, and runtime contracts
already used by imports. It also closes professional point/plant interchange without source-specific
runtime paths.

## Native botanical graph

- [ ] Reuse typed graph-canvas infrastructure while defining a separate botanical type system and IR;
  do not reuse biome/material node semantics by name or JSON shape.
- [ ] Model semantic plant hierarchy and stable element identity for trunks, branches, roots, vines,
  fronds, leaves/needles/blades, flowers, fruit, buds, scars, and dead/broken parts.
- [ ] Provide generators for branching families, phyllotaxis, profile/taper/cross-section, tropisms,
  gravity, light/obstacle response, pruning, graft/attachment, roots, vines, and surface detail.
- [ ] Support hand-drawn spines, node/branch transforms, trimming/pruning, hero-mesh grafts, and
  semantic offsets layered nondestructively over procedural output.
- [ ] Preserve manual edits across parameter changes while their semantic target survives; emit
  visible orphan/conflict diagnostics when topology removes it.
- [ ] Generate coherent family variations and continuous intrinsic age/phenology parameters mapped
  to cooked growth/season/flower/fruit/damaged/dead/harvest states.

These are botanically inspired authoring rules unless a model is separately validated; controls must
be artist-readable and deterministic.

## Complete derived family output

One compile action produces private derived subresources for:

- semantic assembly parts and micro-instance transforms;
- watertight/contoured geometry, UVs, atlases/material assignments, coverage-preserving textures;
- virtual triangle/aggregate-voxel hierarchy and appearance error;
- structural skeleton/spines, wind/interaction weights, modes, limits, and swept-bound metadata;
- collision/breakage and navigation proxies;
- lifecycle/phenotype part/material states;
- RT/OMM derivation inputs; and
- validation thumbnails/statistics/provenance.

The compiler invokes the same Phase-6 cooker as imported plants. No generated mesh/material becomes a
second editable plant source, and no native-only renderer path exists.

## Authoring UX

- [ ] Add structure tree/graph, 3D preview, semantic selection, parameter inspector, family variation
  browser, wind/interaction preview, lifecycle/season timeline, materials/atlas view, collision/nav,
  hierarchy/voxel/error, and validation/cook-stat panels to the Plant workspace.
- [ ] Allow real-time bounded preview and cancellation without changing final deterministic output.
- [ ] Make every graph/manual edit transactional and undoable at semantic-operation granularity.
- [ ] Provide presets/subgraphs through `.splant` internal modules or ordinary plant references with
  explicit interfaces; do not add `.splantgraph`.

## Point and plant interchange

- [ ] Import/export OpenUSD `PointInstancer` positions/orientations/scales/prototypes/stable IDs and
  sparse masks through the canonical point schema, preserving provenance and explicit unsupported
  attributes.
- [ ] Support standard USD skeleton/plant metadata, glTF including `EXT_mesh_gpu_instancing` where
  appropriate, and Houdini point/field attributes as source inputs.
- [ ] Accept standard geometry/skeleton/material exports originating from SpeedTree and preserve
  attribution/reimport settings. Direct proprietary `.st`/`.st9` support is added only after SDK
  license, platform, and redistribution review; it is not promised by this plan.
- [ ] Normalize every source through the Phase-4 plant importer and Phase-6 cooker. Source-specific
  metadata that cannot map is reported, not retained as an alternate runtime object.
- [ ] Export authored fields/points with stable IDs/prototypes for DCC round trips without exporting
  mutable `.svegcell` as authoring truth.

## Acceptance

- [ ] Native and imported versions of equivalent plant structure produce the same normalized family
  schema and render/runtime path.
- [ ] Procedural parameter edits preserve surviving semantic manual offsets and report orphaned ones.
- [ ] Generated family variations, phenotype states, wind rig, collision/nav, virtual hierarchy, and
  materials pass all existing plant validation gates.
- [ ] USD PointInstancer stable IDs/sparse deactivation round-trip without GPU-slot identity leakage.
- [ ] Source reimport never silently loses semantic overrides or creates separately authored generated
  assets.
- [ ] Standard gate, asset-editor E2E, source-license fixtures, and botanical/interchange docs are green.

## NO-LEGACY gate

Imported recipe and native botanical graph are the two source variants inside one `.splant`; they do
not become different asset kinds, cookers, renderers, wind systems, or lifecycle models.

