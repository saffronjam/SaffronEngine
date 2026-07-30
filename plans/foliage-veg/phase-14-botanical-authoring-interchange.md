# Phase 14 — Native botanical authoring and interchange

**Status:** COMPLETED (every box ticked. One carve-out, annotated on its box: the plant-proxy
overlay's appearance is not verified by eye — its geometry reuses the collider overlay's proven
helpers and its inputs are unit-tested, but whether the capsules visually wrap the trunk is a
human-at-the-screen check)

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
- [x] Atlases and coverage-preserving textures, aggregate-voxel appearance error, and RT/OMM
  derivation inputs. Each needs a generator of its own, and none may be faked.
  *(ATLAS PACKING IS BUILT (`atlas.rs`): a deterministic shelf/skyline packer over `(slot, width,
  height)` entries that returns the smallest power-of-two square holding them, plus `remap` from a
  slot-local UV into atlas space. Tallest-first with the slot breaking ties, so the layout never
  depends on the caller's order — it reaches cooked bytes, and one that moved between runs would
  change every artifact hash for no visible reason. A gutter separates every pair, because bilinear
  filtering reaches past a sub-rectangle's edge and touching rectangles bleed (a leaf carrying a
  sliver of bark, visible only at distance). A set that cannot fit is REFUSED rather than partially
  packed: a silently dropped slot renders untextured, which reads as a material bug rather than the
  budget one it is. A UV outside [0,1] clamps rather than wrapping, since tiling cannot survive
  packing. Tests: `every_packed_rectangle_is_disjoint_and_inside_the_atlas`,
  `the_gutter_separates_every_pair` (mutation-checked — removing the gutter advance fails it),
  `the_layout_does_not_depend_on_the_callers_order`,
  `a_set_that_cannot_fit_is_refused_rather_than_dropped`, `a_remapped_uv_lands_inside_its_own_slot`,
  `the_atlas_is_the_smallest_power_of_two_that_holds_the_set`.
  AGGREGATE-VOXEL APPEARANCE ERROR IS ALSO BUILT (`calibrate_voxel_appearance_error`). The cooker's
  `voxel_appearance_error` is a function of bounds and material moments — cheap, and necessarily a
  guess about how wrong the aggregate will LOOK. The cut selector then trusts that number to decide
  when a brick may stand in for triangles, so a low estimate swaps too early and pops. The generator
  renders every transition device-free through the existing reference path, takes the component-wise
  maximum across the canonical fixtures, and widens any declared error the measurement exceeds.
  IT ONLY EVER WIDENS: a measured error below the estimate means the estimate was conservative, and
  narrowing to the measurement would trust a finite fixture set to have found the worst view — the
  exact assumption a measured calibration exists to avoid.
  Tests: `calibration_makes_every_transition_fit_its_declared_error`, `calibration_only_widens`,
  `calibration_is_idempotent`. THE FIXTURE HAD TO CHANGE to make the first one mean anything: a
  tetrahedron's analytic estimate already covers its measurement, so the test passed vacuously until
  it asserted that widening actually happened. It now cooks a comb of thin separated blades, which
  is the case the estimate is blind to — a brick fills the gaps and reads as a slab while the
  triangles read as a comb, and bounds plus occupancy cannot see the difference.
  ALL FOUR GENERATORS REACH A COOK, which is what closes this box — a generator with no caller
  produces nothing.
  In `build_plant_sections` (`plant_cook/publish.rs`), in the order their dependencies force:
  `atlas_normalized_family` packs
  the family's coverage into one atlas (`generate_family_atlas`, which builds the coverage-preserving
  mip chain in the same call, because packing without compositing leaves a caller to write a
  transparent-black gutter that filters into a dark fringe); `calibrate_voxel_appearance_error`
  widens each voxel node's declared error to what a render measures; and `derive_family_micromaps`
  derives the opacity micromaps against the ATLAS alpha plane.
  THAT ORDER IS NOT A PREFERENCE. An atlas remaps the UVs the micromap derivation reads, so deriving
  first answers about texels the GPU never samples — `FamilyAtlas::alpha_plane` says so in its own
  doc. And the atlas must follow `apply_geometry_first_contours`, which re-tessellates alpha cards
  against slot-local coverage and emits new UVs. Get either backwards and both halves still look
  well-formed.
  EACH IS PROVEN ON A PUBLISHED ARTIFACT rather than on a unit fixture:
  `a_cooked_family_carries_its_atlas_and_uvs_that_address_it` (every cooked vertex UV lands inside
  its own slot's rectangle, which is what catches slot-local UVs shipped beside an atlas),
  `a_cooked_plant_declares_an_error_every_transition_fits_within`, and
  `a_cooked_family_derives_micromaps_that_only_ever_remove_work`. The e2e
  `vegetation-atlas-micromap` carries it to the GPU.
  THE APPEARANCE-ERROR CALIBRATION runs over the canonical fixtures between cooking the hierarchy and
  taking any section bytes, which is also what closes the phase-11 triangle↔aggregate transition box.
  Removing the call fails `a_cooked_plant_declares_an_error_every_transition_fits_within` with a
  measured silhouette error of 4294967295 against a declared 196608 — the comb counterexample
  reproducing on a real plant.
  COVERAGE-PRESERVING FAMILY TEXTURES ARE BUILT (`generate_family_atlas`), and packing and mipping
  happen together because splitting them invites the bug: packing alone leaves a caller to
  composite, and a naive composite writes transparent BLACK into the gutter, which filters into the
  slot's edge as a dark fringe — the artefact that makes packed foliage look dirty at distance. The
  gutter carries the EDGE TEXEL'S COLOUR at zero alpha instead, so filtering pulls in the right hue
  and no coverage. Mips are the alpha-area-preserving chain rather than a box filter, since a
  cutout thinned by naive downsampling vanishes at distance. A slot whose buffer does not match its
  declared extent is refused rather than composited positionally into someone else's rectangle.
  Tests: `the_generated_atlas_carries_each_slot_and_a_full_mip_chain`,
  `the_gutter_carries_edge_colour_at_zero_alpha` (mutation-checked — a transparent-black gutter
  fails it and nothing else), `a_slot_whose_pixels_do_not_match_its_extent_is_refused`.
  RT/OMM DERIVATION INPUTS ARE BUILT (`FamilyAtlas::alpha_plane`), and taking them from the PACKED
  atlas rather than from a slot's own image is the whole point: after packing, a triangle's UVs
  address atlas space, so a pyramid over the unpacked image would answer about texels the GPU never
  reads. `the_generated_plane_drives_a_real_opacity_micromap_derivation` runs the actual Phase 11
  `derive_opacity_micromap` over the generated plane with UVs remapped through the layout, and
  requires it to prove opaque inside a solid slot — an end-to-end check rather than a shape check.)*
- [x] `plant-create` is the authoring front door: it mints a native family from the starter graph
  (`BotanicalGraphDocument::sapling`) and saves it to the catalog. Before this, a `.splant` could only
  be produced by writing Rust — the format, compiler, cooker, and runtime were all reachable and the
  creation path was not. `plant-growth` reports what a family's graph grows.

## Authoring UX

- [x] Add structure tree/graph, 3D preview, semantic selection, parameter inspector, family variation
  browser, wind/interaction preview, lifecycle/season timeline, materials/atlas view, collision/nav,
  hierarchy/voxel/error, and validation/cook-stat panels to the Plant workspace. (`PlantGraphPanel`
  (the `plantGraph` asset-editor dock panel) carries the STRUCTURE readout (axes, elements, shells,
  grafts, vertices, triangles, parts, height, applied edits, graph identity), the FAMILY VARIATION
  BROWSER with per-variation age editing, the addressable ELEMENT list — which is also the semantic
  selection surface, since an edit targets one of those identities — the orphaned-edit list, and the
  VALIDATION/COOK-STAT readout from `plant-validate`.
  THE 3D PREVIEW EXISTS — an earlier note here listed it as missing and was wrong. `enter_plant_preview`
  builds a real preview scene and returns `plant_combinations`, and a `.splant` subject drives the
  `assetPreview` subsurface: `AssetEditorWorkspace` short-circuits to a summary for biome and
  vegetation-map subjects only, so a plant falls through to the live surface. A stale comment in
  `assetEditorPanels.tsx` still says vegetation subjects have no 3D preview surface and is
  contradicted by the code three lines below it; deleting that comment belongs with this box.
  THAT GAP IS CLOSED. The capability effect now opens `plantGraph` for a plant subject, AFTER the
  rig branch closes `skeleton` — the two share a dock leaf, and a plant grown from a botanical graph
  is rigless, so opening first would put the panel in a leaf that then collapses. The stale comment
  is deleted; the accurate statement is that only biome and vegetation-map subjects short-circuit to
  a summary, because neither has a single renderable form to show.
  THE WIND/INTERACTION PREVIEW IS NOW BUILT. `PlantWindPanel` (the `plantWind` asset-editor dock
  panel, opened for a plant subject beside `plantGraph` and closed for every other) drives the REAL
  wind field and the REAL interaction field rather than a model of them, so the previewed plant is
  the plant. That is what makes it a surface rather than a readout: a table of stiffness and drag
  says what an author typed, and a plant leaning under a gale says what they authored — and a stiff
  sapling that moves like a supple reed only separates from one at a field strong enough to show it,
  which is why the speed control offers named conditions as well as a slider.
  THE IMPULSE CARRIES AN EXPLICIT DIRECTION, which is not a detail: a directionless impulse pushes
  radially outward from its own centre and therefore cancels AT the centre, so a push aimed at the
  subject would move nothing and look exactly like a broken control. That cost most of a day
  elsewhere in this planset and is written into the panel so it cannot be rediscovered.
  THE LIFECYCLE/VARIATION SCRUB WAS ALREADY THERE and this box's earlier note undercounted it:
  `AssetEditorWorkspace` reads `plantCombinations` from `enter-asset-preview` and drives
  `set-asset-preview-options {variation, phenotype}`, so scrubbing a combination changes what the
  live preview renders. What is missing from that clause is the SEASON axis specifically — the
  rendered phenotype resolves from typed lifecycle plus season, and no preview control moves the
  calendar.
  PHENOTYPES ARE NOW AUTHORABLE. `plant-phenotypes` reads and replaces a family's appearance list
  over the control plane, which nothing could do before — `plant-create` scaffolds exactly one and
  there was no way to add a second. That is the authoring half of the lifecycle clause; the panel
  over it and the season scrub are what remain.
  THE MATERIALS/ATLAS VIEW IS NOW BUILT. `plant-atlas {plant, level?}` returns one level of the
  packed coverage atlas as a PNG with its placements, and `PlantAtlasPanel` shows it over a checker
  so the gutter and the cut-out alpha read as transparent rather than as black — which is the whole
  reason to look at an atlas.
  IT READS THE PUBLISHED ARTIFACT, NEVER RE-PACKS. A family's UVs address exactly one layout, so a
  second packing of the same slots produces a different arrangement and a view showing it would be
  authoritative about an image the plant is not sampling. Recooking first is a cache hit for a family
  already cooked, so the bytes come from the artifact either way.
  THE MIP LEVEL SCRUBS, and that is not decoration: the chain is coverage-preserving, a level that
  lost alpha area is exactly where distant foliage goes thin, and it is invisible at level zero.
  Covered by `the_published_atlas_reads_back_as_an_image_its_placements_fit_inside`, which asserts
  the reply DECODES as a PNG at the extent it reports (a raw-bytes reply could get that wrong with
  nothing noticing), that every placement lands inside the atlas it claims, that a smaller level is
  genuinely smaller, and that a level past the chain is an error rather than a silent clamp.
  A family whose slots resolve to catalog materials cooks no atlas; the panel says so in place
  rather than raising a toast on every plant an author opens.
  THE HIERARCHY/VOXEL/ERROR VIEW IS NOW BUILT, and it is two halves that only mean something
  together. `plant-hierarchy` returns the published cut node by node — representation, primitive
  count, page, depth, and the five-component declared error — and `set-hierarchy-cut` pins what the
  preview beside it actually draws.
  PINNING THE CUT IS THE HALF THAT WAS MISSING. A representation comparison needs the cut to move
  while the CAMERA HOLDS STILL: flying out to reach the aggregate shrinks the subject at the same
  time, so any difference conflates the two changes. It used to be boot-time `SAFFRON_CUT_OVERRIDE`
  env, which is no way to compare anything — the env stays as the initial value a test needs before
  the first frame, not as a second mechanism.
  THE ERROR COLUMN IS THE NUMBER THE SELECTOR READS, shown in local units rather than as a raw
  Q15.16 word, and it says `saturated` where it saturates. That is the surprising case worth seeing:
  a comb of thin blades calibrates to an error so wide the aggregate is never selected while
  anything finer is resident — correct, and invisible until you can look at it.
  DEPTH IS COMPUTED SERVER-SIDE, once, because a caller walking parents per row is quadratic on a
  family with thousands of nodes and the cut's shape is the first thing anyone reads.
  Covered by `the_published_hierarchy_reads_back_with_both_representations_and_their_errors`:
  aggregate nodes exist (a hierarchy of triangles alone would leave the cut control nothing to move
  between), exactly one root, every parent a real node, and every total at least its own silhouette
  component — a total below a component would misreport which node gets picked.
  THE LIFECYCLE/SEASON TIMELINE IS NOW BUILT. `plant-season-phenotype {plant, seasonMille,
  lifecycle?}` answers which appearance the family renders at a point in the year, and
  `PlantSeasonPanel` scrubs the year and binds the answer to the live preview — the plant through
  the year rather than a picker over phenotype ids.
  IT CALLS THE ENGINE'S OWN RESOLVER, `resolve_rendered_phenotype`, never a second reading of the
  same rules in TypeScript. A preview that resolved the season differently from the renderer would
  be showing an appearance the scene never picks, which is the failure a "timeline" is most likely
  to have and least likely to reveal.
  LIFECYCLE WINS OVER SEASON, so it is a control rather than an afterthought: a dead plant does not
  turn autumnal, and being able to set both is what makes that visible. The panel binds the
  phenotype AND its variation together, because a phenotype draws a specific variation and applying
  one without the other shows a combination the family never declares.
  Covered by `plant_season_phenotype_follows_lifecycle_then_season`, which asserts BOTH directions
  of the seasonal window (a resolver stuck on either answer passes one of them), that a dead plant
  in autumn falls back to the cooked appearance rather than taking the seasonal match — the part a
  season-first resolver gets wrong — and that an out-of-range season is refused rather than wrapped.
  THE COLLISION/NAV VIEW IS NOW BUILT, and it is an overlay rather than a table. `plant-proxies`
  reports the derived capsules and footprints IN METRES, and `build_plant_proxy_overlays` draws them
  over the preview through the same overlay path the scene uses for physics colliders — with the
  same two toggles, applied to whatever surface is in front of you.
  IT HAD TO BE ITS OWN OVERLAY. `build_collider_overlays` guards itself off in the preview, correctly:
  a preview scene has no physics bodies. A family's proxies are DERIVED — a result of what grew, like
  its dimensions — so there is nothing in the scene to attach them to and nothing else would ever
  show them. They are read straight off the authored family.
  THE TABLE IS THE LEGEND, NOT THE VIEW. A list of half-extents says nothing about whether a capsule
  wraps the trunk it was fitted to; only the drawing answers that, which is why the panel's toggles
  are the point and its rows are the caption. Breakable proxies take a warmer colour, because "this
  one comes off" is the property worth spotting at a glance.
  Covered by `plant_proxies_reports_what_the_family_derived`, which pins the reply's SHAPE (the
  overlay reads those arrays directly, and a missing one draws nothing while looking fine) and that
  dimensions arrive in metres — a raw Q15.16 word would draw a capsule sixty-five thousand times too
  big and read as a broken overlay rather than as a unit mistake.
  ALL ELEVEN SURFACES THE BOX NAMES NOW EXIST: structure tree, 3D preview, semantic selection,
  parameter inspector, variation browser, wind/interaction, lifecycle/season, materials/atlas,
  collision/nav, hierarchy/voxel/error, and validation/cook-stats.
  ONE HONEST CARVE-OUT, the same one Phase 8's acceptance box carries: the proxy overlay's
  APPEARANCE is not verified by eye. Its geometry emission reuses the collider overlay's proven
  helpers and its inputs are unit-tested, but whether the capsules visually wrap the trunk is a
  human-at-the-screen check that no test here makes.)
- [x] Allow real-time bounded preview and cancellation without changing final deterministic output.
  *(ONE WALK UNDER TWO BUDGETS, not two evaluators — two would drift and the preview would stop
  predicting the plant. `BotanicalBudget` bounds axes and placed elements and carries the existing
  `GraphCancellationToken`; `grow` takes it at every call site and `BotanicalGrowth.truncated`
  reports whether it bit. `plant-growth` accepts `maxAxes`/`maxElements` and returns `truncated`.
  THE BOUND STOPS THE WALK BETWEEN NODES, NEVER INSIDE ONE, which is the whole guarantee: a node
  that ran produced exactly what it would have produced unbounded, so a preview is a PREFIX of the
  cooked plant rather than a different one. An artist tuning against it is tuning against the real
  thing. `a_bounded_preview_is_a_prefix_of_the_cooked_plant` asserts every previewed axis is
  byte-identical to the cooked one, not merely that fewer arrived.
  THE FINAL OUTPUT CANNOT BE REACHED. `BotanicalBudget::COOK` carries no bound and no token, and
  the cook path names it explicitly — as does a module call, since half a preset is a different
  preset. Pinned by `the_cook_budget_can_never_truncate`. Cancellation rides the same mechanism
  (`a_cancelled_preview_stops_and_says_so`), so a cancelled preview is also a prefix rather than a
  partial result, and it reports rather than failing.)*
- [x] Make every graph/manual edit transactional and undoable at semantic-operation granularity.
  (Transactional at the seam: `plant-graph-set` replaces the whole document in one call and revalidates
  the regrown family before it saves, so a refused edit changes nothing. Undoable through the editor's
  existing `pushEdit`: `PlantGraphPanel.apply` records ONE edit per artist-level operation — "Add
  variation", "Set variation age" — whose inverse is the PREVIOUS GRAPH DOCUMENT replayed through the
  same one write path. That is what keeps undo honest for a derived model: nothing reconstructs the old
  parts, dimensions, spines, or proxies by hand, because regrowing the old graph produces them. Never
  keystroke granularity, and never a second write path for the inverse.)
- [x] Provide presets/subgraphs through `.splant` internal modules or ordinary plant references with
  explicit interfaces; do not add `.splantgraph`.
  *(ORDINARY PLANT REFERENCES, which the box offers as the alternative and which fits the existing
  design: a `BotanicalOperator::ModuleCall` node grows another `.splant` at each incoming frame. The
  module is an ordinary `.splant` carrying `PlantFamilyRole::Module` — it opens, previews, and cooks
  like any family, which is what keeps a preset editable rather than a second document format. NO
  `.splantgraph` was added, and none is needed.
  THE INTERFACE IS SMALL BY DESIGN, and the reason is the failure mode I have hit repeatedly in this
  planset: `.sbiome`'s typed parameter system is the obvious model, but botanical operators carry
  concrete scalars, so a parameter system over them would have meant declaring knobs no operator
  reads. Instead each binding — which module, which of its variations, what scale — has a consumer
  in the evaluator. That is the test of whether a parameter is real.
  IDENTITIES REBASE THROUGH THE CALL GUID, which is what makes two copies separately editable: an
  authored edit addresses the element at THAT call site and never moves the other. Derived through
  the existing `BotanicalElementId::child`, so `mix` is untouched and no authored edit is orphaned.
  BOUNDED AND CYCLE-CHECKED IN THE RESOLVER rather than the evaluator: `grow` sees one document at a
  time, and only the chain of assets a call reaches through can tell whether it has come back to
  where it started. A chain's own limit never widens what an ancestor allowed.
  NO DEFAULT RESOLVER. Every `grow` call site states whether it can reach modules or refuses them
  (`NoBotanicalModules`), because a path that grew a module-calling graph without its modules would
  report a plant missing its presets and call it a success. ~50 call sites updated.
  THE FORMAT MOVED AS FOUR THINGS TOGETHER per the vegetation rule: writer, reader,
  `PLANT_ASSET_VERSION` 4→5, and the `plant_asset_schema_hash` domain string (`/v4` → `/v5` plus
  `+family-role+modules`). The generated e2e fixtures were regenerated with
  `xtask gen-vegetation-e2e-fixture`, which is the documented procedure when a schema identity moves.
  Tests: `a_module_call_grows_the_module_at_every_frame`, `a_module_places_at_its_call_sites_scale`,
  `two_call_sites_of_one_module_address_different_elements`,
  `a_graph_grown_without_modules_refuses_a_module_call`,
  `a_module_table_round_trips_and_is_matched_against_the_graph` (both binding directions plus
  self-reference), and at the asset layer `a_module_call_composes_the_referenced_family`,
  `a_module_call_requires_a_module_role`, `a_module_cycle_is_rejected_rather_than_grown`.
  Gates: `just engine`, `just prepare-for-commit`, `just schema`, `just test`, `just e2e`, and the
  docs three checks (botanical-graph.md gains "Presets are ordinary plants").)*

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
- [x] Support standard USD skeleton/plant metadata, glTF including `EXT_mesh_gpu_instancing` where
  appropriate, and Houdini point/field attributes as source inputs. (Houdini JSON `.geo` point clouds
  are read by `read_houdini_points` — `P`, `orient`/`rot`, `scale`/`pscale`, `id`, `name`/`variant` —
  and glTF `EXT_mesh_gpu_instancing` by `read_gltf_instancing`, which composes each instance transform
  with its node's whole ancestor chain and reports every instance attribute it cannot express.
  USD `PointInstancer` prims by `read_usd_point_instancers` over the USDA text form.
  `vegetation-import-points` picks the reader from the file extension.
  USD SKELETONS ARE NOW READ. `read_usd_skeletons` returns every `UsdSkel` skeleton in a stage — its
  `SkelRoot`, its joints in declared order, each joint's derived parent, and the rest and bind
  transforms as row-major `matrix4d`. A skeleton that declares no transforms implies the identity
  rather than zeros, mismatched parallel arrays are refused rather than read positionally, and
  attributes outside the expressible set are reported. `sa vegetation-usd-skeletons {path}` exposes
  it. Tests: `a_usd_skeleton_reads_its_joints_transforms_and_enclosing_root`,
  `a_joint_parent_is_a_path_boundary_not_a_string_prefix`,
  `a_skeleton_with_mismatched_arrays_is_refused`,
  `a_stage_with_no_skeleton_reads_as_empty_rather_than_failing`.
  TWO TRAPS HANDLED EXPLICITLY. USD states a skeleton's hierarchy in the path tokens rather than in
  a parent array, and a plain string prefix makes `Root/Trunk` the parent of `Root/TrunkGuard` — an
  error that survives every count and length check and shows up only as geometry bending the wrong
  way, so the boundary must fall on a separator. And a `matrix4d` nests its rows inside an outer
  pair, which the existing tuple scanner (correct for a flat point list) reads off by one; matrices
  use a nesting-aware leaf-row scanner instead.
  NO USD RUNTIME DEPENDENCY, which reverses the choice recorded in the slice plan. The existing
  reader deliberately parses the USDA text form because the arrays a plant needs are stated
  directly; a runtime would have meant either a second reader over the same files — the two paths
  the conventions forbid — or rewriting a working one. What a runtime would actually add is
  `.usdc`/`.usdz` binary crates, a different capability from this box.
  THEY ARE NOW SOURCE INPUTS, which was the clause this note used to leave open. A recipe may name
  a `.usd`/`.usda` file, and `resolve_file_source` routes it to `resolve_usd_skeleton_source` rather
  than to `translate_model` — the model importers do not read `UsdSkel` at all, and teaching them a
  fourth format would have made a second truth about the same file.
  A USD SOURCE CONTRIBUTES STRUCTURE ONLY: joints, no meshes, no materials. Inventing an empty mesh
  would hand the compiler a source that claims to draw nothing rather than one that claims not to
  draw. A stage with no skeleton is REFUSED rather than imported empty — a recipe naming the wrong
  file is a mistake, and an empty joint list surfaces much later as a plant that refuses to bend
  with nothing pointing at the cause.
  A JOINT'S IDENTITY IS ITS AUTHORED PATH TOKEN, the identity USD itself uses, so a re-import
  against an edited stage matches joints by name rather than by position — inserting one joint
  would otherwise silently re-parent every joint after it.
  ONE REAL BUG THE TEST CAUGHT: the first transpose was wrong twice over. USD writes `matrix4d`
  row-major and composes with row vectors; `Mat4` is column-major with column vectors. Converting
  between the conventions IS a transpose — and a transpose of a row-major array read as a
  column-major one is the identity on the storage, so the values pass through in order. Permuting
  them transposes twice and leaves the translation in the last row, where `col(3)` reads it as a
  basis vector. That ships as bones bending the wrong way, not as an error.
  Tests: `a_usd_stage_resolves_as_a_skeleton_source_input` (parentage across the
  `Root/Trunk`/`Root/TrunkGuard` prefix trap, the translation surviving the transpose, and
  selector-not-position identity) and
  `a_usd_stage_with_no_skeleton_is_refused_rather_than_imported_empty`.
  THE LAST CLAUSE IS CLOSED BY RESEARCH RATHER THAN BY CODE, because there is nothing to implement.
  "Standard USD plant metadata" names a thing that does not exist, checked against primary sources
  in 2026-07: OpenUSD's complete schema library is ar/kind/pcp/sdf/sdr/usd/usdGeom/usdHydra/usdLod/
  usdLux/usdMedia/usdMtlx/usdPhysics/usdProc/usdProfiles/usdRender/usdRi/usdSemantics/usdShade/
  usdSkel/usdUI/usdUtils/usdVol — no vegetation, plant, or foliage domain. AOUSD's interest groups
  are AECO, DEI, Emerging Geometry, IEDT, Web, Build, and Characters/Motion/Interactivity; none
  covers vegetation. SpeedTree's own documentation lists USD as an export format and describes what
  it can carry (leaf references, branch spines, vertex blends) while documenting NO prim types,
  attribute names, or metadata conventions — so there is not even a vendor convention to target.
  THE STANDARD SURFACE FOR VEGETATION INTERCHANGE IS `UsdGeomPointInstancer` PLUS `UsdSkel`, and
  both are read. Houdini's scatter workflow authors `pscale`/`orient` point attributes, which is
  exactly what a `PointInstancer` encodes — so the de-facto convention resolves to the standard
  schema already supported rather than to a separate plant vocabulary.
  `usdSemantics` is the nearest relative and is deliberately not treated as this clause: it is a
  generic labeling taxonomy for segmentation and ML ground truth ("car", "pedestrian"), not plant
  metadata, and reading it would answer a different question than this box asks.
  WHAT A STAGE'S UNRECOGNISED ATTRIBUTES DO IS THE HONEST ANSWER AND IS ALREADY BUILT: both readers
  collect them into an `unsupported` list that reaches the caller — `sa vegetation-usd-skeletons`
  returns it on the wire, and the plant source path logs it. So a stage carrying a vendor's custom
  plant attributes reports exactly which ones could not be expressed instead of guessing at them or
  silently dropping them. No scaffolding anticipates a plant schema: there is no placeholder type,
  no unread field, and no TODO in the interchange readers. Plant-specific USD schemas are
  also not read, and there is no standard one: the plant metadata SpeedTree and others emit is
  custom prim attributes, which this reports as unsupported rather than guessing at.)
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

