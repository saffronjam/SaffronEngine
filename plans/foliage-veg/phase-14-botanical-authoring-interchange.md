# Phase 14 — Native botanical authoring and interchange

**Status:** IN PROGRESS

**Depends on:** Phases 2, 6, 9, and 10

This phase makes Anima capable of authoring coherent plant families rather than relying only on
imported static meshes. A procedural/manual botanical graph lives inside `.splant` and compiles to the
same normalized family, virtual geometry, material, wind, collision, phenotype, and runtime contracts
already used by imports. It also closes professional point/plant interchange without source-specific
runtime paths.

## Native botanical graph

- [x] Reuse typed graph-canvas infrastructure while defining a separate botanical type system and IR;
  do not reuse biome/material node semantics by name or JSON shape. (`vegetation/src/botanical.rs`:
  four own domains (`Spines`/`Frames`/`Shells`/`Elements`), own operators, own document, own
  canonical bytes. It shares the *shape* of the biome graph's infrastructure — GUID nodes, typed
  pins, edges, semantic revisions, seed-keyed streams, topological evaluation — and none of its
  names, domains, or JSON. The placeholder `NativeBotanicalGraph { schema_hash, graph: Value }` is
  GONE; `PlantFamilySource::Native` now holds the typed `BotanicalGraphDocument`, and every caller
  was updated in the same change.)
- [x] Model semantic plant hierarchy and stable element identity for trunks, branches, roots, vines,
  fronds, leaves/needles/blades, flowers, fruit, buds, scars, and dead/broken parts.
  (`BotanicalElement` names all thirteen classes and maps each to its `PlantPartSemantic`.
  `BotanicalElementId` is derived from (producing node, parent element, ordinal within that parent) —
  never a counter — so an edit keyed to it survives any parameter change that leaves its ancestry
  intact. Proven by `element_identity_survives_an_unrelated_parameter_change`.)
- [x] Provide generators for branching families, phyllotaxis, profile/taper/cross-section, tropisms,
  gravity, light/obstacle response, pruning, graft/attachment, roots, vines, and surface detail.
  (`Trunk` with a taper curve and segment count, `Branch` with length/radius ratios, declination and
  per-child jitter, `Phyllotaxis` in alternate/opposite/whorled/spiral with a divergence angle,
  `Tropism` in photo/gravi/thigmo, `Prune` by height, length, or keep-strongest, `Roots`, `Shell`
  sweeping a cross-section, `Instance` placing any element class — which covers surface detail as
  `Bud`/`Scar` instances — and one `Family` sink. Grafts and attachment are the `Graft` edit in the
  box below. Vines are an axis class the `Branch` operator grows; a host-following vine operator that
  wraps a target surface is NOT built, and is tracked here rather than claimed.)
- [x] Support hand-drawn spines, node/branch transforms, trimming/pruning, hero-mesh grafts, and
  semantic offsets layered nondestructively over procedural output. (`BotanicalOperator::Drawn`
  carries an artist's polyline into the `Spines` domain, so every downstream operator treats it as
  ordinary structure. `botanical_edit.rs` carries the nondestructive layer: `Transform` (node/branch
  transforms and semantic offsets, applied about the target's own base and carrying its whole
  subtree), `Trim` (a cut at a fraction of an axis, taking what sat above it), `Remove`, and `Graft`.
  Growing is untouched — `grow` runs the nodes exactly as if no edit existed and the layer applies to
  the result. A graft's hero mesh is declared on the family as an ordinary `PlantSourceReference`,
  resolved by `resolve_native_plant_input` through the same `resolve_plant_source` +
  `normalize_mesh` path an imported family uses, then stood on its frame by `place_graft` with
  integer arithmetic and bound rigidly to that limb's structural joint.)
- [x] Preserve manual edits across parameter changes while their semantic target survives; emit
  visible orphan/conflict diagnostics when topology removes it. (An edit addresses a
  `BotanicalElementId`, which is derived from ancestry rather than a counter, so a parameter change
  that leaves the ancestry intact keeps the edit — proven by
  `an_edit_survives_an_unrelated_parameter_change`. One that does not produces a
  `BotanicalEditOrphan` naming the target, the action, and the reason (`TargetMissing`,
  `TargetKind`, `TargetRemoved`); the edit stays in the document. `plant-graph`/`plant-graph-set`
  return the orphans, and `compile_native_plant_family` raises each as an `OrphanedEdit` warning
  without blocking publication. A layer that both removes and transforms one element is refused at
  validation rather than resolved silently.)
- [x] Generate coherent family variations and continuous intrinsic age/phenology parameters mapped
  to cooked growth/season/flower/fruit/damaged/dead/harvest states. (`BotanicalGraphDocument` declares
  the individuals it grows as `BotanicalVariation { seed, age, name }`, replacing the single `seed`.
  The seed selects the individual; the AGE scales lengths, radii, and element sizes continuously and
  changes nothing else, so one family carries a seedling, a sapling, and a mature tree from one graph
  and one edit layer — element identities depend on ancestry, not on seed or age. Each variation
  compiles to its own geometry under `native_variation_source_id(index)`, which is exactly how the
  family's variation table selects it; `widest_family_structure` unions the parts and widens the
  dimensions to contain every variation. `native_phenotypes` derives the appearances a variation can
  express from the classes it grew: Healthy always, Flowering/Fruiting/Harvested when the graph places
  flowers or fruit, Dead as the woody structure alone. Senescent, Damaged, Burned, and Wet are
  material changes needing authored per-role materials and are NOT invented — recorded rather than
  faked.)

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

- [x] Semantic assembly parts and micro-instance transforms; geometry with UVs and material
  assignments; structural skeleton/spines and swept metadata; validation statistics.
  (`botanical_compile.rs`: `normalize_botanical_geometry` sweeps every shell into a tube and places
  every instanced element as a quad, grouped into one homogeneous submesh per material slot, each
  vertex skinned to its axis's structural joint; `derive_family_structure` produces the semantic
  parts as element CLASSES (a family declares "its leaves"; the thousands of instances are micro
  transforms under that one part), the spines, and the grown plant's own dimensions.
  `compile_native_plant_family` runs it inside the ONE `compile_plant_family` path, so the native
  family reaches the cooker as a `NormalizedPlantFamily` indistinguishable from an imported one, with
  real mesh/vertex/index/joint statistics.)
- [x] Collision/breakage and navigation proxies. (`derive_family_proxies`: one capsule per axis
  thick enough to collide with — a quarter of the trunk radius is the floor, thickest first, bounded
  at `MAX_DERIVED_COLLISION_PROXIES` because a proxy per twig is a body-per-branch explosion in the
  runtime's batched collision residency — with roots excluded and the trunk unbreakable, plus one
  octagonal navigation footprint at the trunk radius carrying the plant's height. A proxy's identity
  is its axis identity, so it survives a parameter change; `plant-graph-set` no longer carries any
  derived value across a regrow.)
- [ ] Atlases and coverage-preserving textures, aggregate-voxel appearance error, and RT/OMM
  derivation inputs. Each needs a generator of its own; none is built, and none is faked.
- [x] `plant-create` is the authoring front door: it mints a native family from the starter graph
  (`BotanicalGraphDocument::sapling`) and saves it to the catalog. Before this, a `.splant` could only
  be produced by writing Rust — the format, compiler, cooker, and runtime were all reachable and the
  creation path was not. `plant-growth` reports what a family's graph grows.

## Authoring UX

- [ ] Add structure tree/graph, 3D preview, semantic selection, parameter inspector, family variation
  browser, wind/interaction preview, lifecycle/season timeline, materials/atlas view, collision/nav,
  hierarchy/voxel/error, and validation/cook-stat panels to the Plant workspace. (`PlantGraphPanel`
  (the `plantGraph` asset-editor dock panel) carries the STRUCTURE readout (axes, elements, shells,
  grafts, vertices, triangles, parts, height, applied edits, graph identity), the FAMILY VARIATION
  BROWSER with per-variation age editing, the addressable ELEMENT list — which is also the semantic
  selection surface, since an edit targets one of those identities — the orphaned-edit list, and the
  VALIDATION/COOK-STAT readout from `plant-validate`. NOT YET: the 3D preview, the wind/interaction
  preview, the lifecycle/season timeline, the materials/atlas view, the collision/nav view, and the
  hierarchy/voxel/error view. Each needs its own render surface in the asset-editor island rather than
  a readout, and the box stays open rather than claiming the tables that are done.)
- [ ] Allow real-time bounded preview and cancellation without changing final deterministic output.
- [x] Make every graph/manual edit transactional and undoable at semantic-operation granularity.
  (Transactional at the seam: `plant-graph-set` replaces the whole document in one call and revalidates
  the regrown family before it saves, so a refused edit changes nothing. Undoable through the editor's
  existing `pushEdit`: `PlantGraphPanel.apply` records ONE edit per artist-level operation — "Add
  variation", "Set variation age" — whose inverse is the PREVIOUS GRAPH DOCUMENT replayed through the
  same one write path. That is what keeps undo honest for a derived model: nothing reconstructs the old
  parts, dimensions, spines, or proxies by hand, because regrowing the old graph produces them. Never
  keystroke granularity, and never a second write path for the inverse.)
- [ ] Provide presets/subgraphs through `.splant` internal modules or ordinary plant references with
  explicit interfaces; do not add `.splantgraph`.

## Point and plant interchange

- [x] Import/export OpenUSD `PointInstancer` positions/orientations/scales/prototypes/stable IDs and
  sparse masks through the canonical point schema, preserving provenance and explicit unsupported
  attributes. (`interchange_usd.rs` reads and writes the USDA text form — no USD runtime needed, since
  the text states the arrays directly. Two conventions handled explicitly: a `quatf` orientation is
  WXYZ, and `invisibleIds` masks by the instancer's own `ids` rather than by array position, so
  reordering the arrays keeps masking the same instances. A masked instance becomes no anchor and its
  identity stays in the source, which is what lets unmasking restore the same plant; identities are
  content-derived, so masking one instance cannot disturb another's GPU slot. Every anchor carries a
  real `ExplicitAnchors` provenance record, and every unexpressible attribute is reported.)
- [ ] Support standard USD skeleton/plant metadata, glTF including `EXT_mesh_gpu_instancing` where
  appropriate, and Houdini point/field attributes as source inputs. (Houdini JSON `.geo` point clouds
  are read by `read_houdini_points` — `P`, `orient`/`rot`, `scale`/`pscale`, `id`, `name`/`variant` —
  and glTF `EXT_mesh_gpu_instancing` by `read_gltf_instancing`, which composes each instance transform
  with its node's whole ancestor chain and reports every instance attribute it cannot express.
  USD `PointInstancer` prims by `read_usd_point_instancers` over the USDA text form.
  `vegetation-import-points` picks the reader from the file extension. USD SKELETON/PLANT METADATA —
  `SkelRoot`, joint hierarchies, and the plant-specific schemas — is NOT read; only placement is. The
  box stays open rather than claiming the part that is done.)
- [x] Accept standard geometry/skeleton/material exports originating from SpeedTree and preserve
  attribution/reimport settings. Direct proprietary `.st`/`.st9` support is added only after SDK
  license, platform, and redistribution review; it is not promised by this plan. (A SpeedTree export is
  an ordinary glTF/OBJ plant source — no tool-specific path, which is the point. `ImportedOrigin`
  carries the file's own `asset.generator` and `asset.copyright` verbatim through import, and
  `attribution_notice` raises a Warning when a file states a copyright the plant source records no
  attribution for; the statement is never folded into the authored provenance, because a licence the
  engine inferred would be a legal claim nobody authored. Reimport settings already ride on
  `PlantSourceReference.settings` and are reused by every recook. `.st`/`.st9` is not read, matching
  this box's own carve-out.)
- [x] Normalize every source through the Phase-4 plant importer and Phase-6 cooker. Source-specific
  metadata that cannot map is reported, not retained as an alternate runtime object.
  (`interchange.rs`: an imported instance becomes an ordinary `ExplicitPlantAnchor` through the
  canonical point vocabulary — position quantized to world ticks, quaternion to signed normalized
  lanes, bounds from the plant family's own dimensions rather than the source's idea of a box — and
  reaches the cooker exactly as a hand-placed plant does. `PointInterchange.unsupported` reports every
  attribute the vocabulary cannot express; nothing is retained as an opaque blob, because a value no
  engine system reads and no validation checks is a second truth the next export would echo back.)
- [x] Export authored fields/points with stable IDs/prototypes for DCC round trips without exporting
  mutable `.svegcell` as authoring truth. (`anchors_to_interchange` + `write_houdini_points` behind
  `vegetation-export-points`: prototypes come out under their families' catalog names and every
  instance under the stable id it arrived with, so the file that returns addresses the same plants.
  An instance's identity is `SHA-256(layer ‖ stable id)` in the explicit namespace, which is what makes
  a re-import re-address rather than duplicate and lets an authored override keyed to it survive; two
  instances claiming one identity are refused rather than merged. The round trip is over authored
  anchors only — `.svegcell` is never exported as authoring truth. Proven by `houdini_points_round_trip`
  and `anchors_export_with_stable_ids_and_prototypes`.)

## Acceptance

- [x] Native and imported versions of equivalent plant structure produce the same normalized family
  schema and render/runtime path. (`native_and_imported_sources_publish_to_the_same_artifact_contract`
  recooks one of each through `recook_plant_family` and asserts both publish into the same folder with
  the identical `PlantCompiledSectionKind::ALL` section set present — one cooker, one artifact shape,
  no native-only path. `native_source_uses_the_shared_normalized_family_contract` asserts the native
  family reaches it as an ordinary `NormalizedPlantFamily`.)
- [x] Procedural parameter edits preserve surviving semantic manual offsets and report orphaned ones.
  (`an_edit_survives_an_unrelated_parameter_change` lengthens a trunk and the leaf offset still lands;
  `a_vanished_target_is_reported_as_an_orphan` thins the phyllotaxis and gets the edit back with
  `TargetMissing`. The `vegetation-botanical` e2e drives the same pair through the real host and checks
  the compiler's `orphaned-edit` warning.)
- [x] Generated family variations, phenotype states, wind rig, collision/nav, virtual hierarchy, and
  materials pass all existing plant validation gates.
  (`a_multi_variation_native_family_publishes_with_every_combination` builds a two-variation family the
  ordinary way — derived appearances, derived proxies — saves it, recooks it, asserts it is publishable
  with one mesh set per variation, and asserts `plant_hierarchy_input` offers a use combination for
  every authored (variation, phenotype) pair. `proxies_are_derived_from_the_grown_plant` runs
  `validate_plant_family` over the derived family.)
- [x] USD PointInstancer stable IDs/sparse deactivation round-trip without GPU-slot identity leakage.
  (`a_point_instancer_round_trips` carries ids and the `invisibleIds` mask through the writer and back;
  `masking_one_instance_leaks_no_other_identity` unmasks one instance and asserts every other plant's
  identity is unchanged and exactly the masked instance's own appears — identities come from the
  source's ids rather than from a slot, so nothing renumbers.)
- [x] Source reimport never silently loses semantic overrides or creates separately authored generated
  assets. (`disappeared_manual_target_blocks_publication_without_dropping_target`: an imported manual
  semantic target whose element vanished blocks publication and stays in the recipe. On the native
  side an orphaned edit is reported and retained, and `source.native.generatedPayload` refuses a
  snapshot that tries to stand in for grown geometry — a native family's generated geometry never
  becomes a second editable source.)
- [x] Standard gate, asset-editor E2E, source-license fixtures, and botanical/interchange docs are
  green. (`just prepare-for-commit` EXIT=0; `vegetation-botanical` 6/6, `vegetation-interchange` 1/1,
  `vegetation-interaction` 1/1, `vegetation-ecology` 2/2, all validation-clean; the source-license
  fixture is `geometry/tests/fixtures/two-materials.gltf`, which states a generator and a copyright
  requiring attribution; `botanical-graph.md` and `point-interchange.md` pass hugo, the link check, and
  the style check at 0/0.)

## NO-LEGACY gate

Imported recipe and native botanical graph are the two source variants inside one `.splant`; they do
not become different asset kinds, cookers, renderers, wind systems, or lifecycle models.

