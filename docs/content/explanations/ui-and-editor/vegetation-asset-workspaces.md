+++
title = 'Vegetation asset workspaces'
weight = 20
+++

# Vegetation asset workspaces

Opening a plant, biome, or vegetation-map row from the [Assets panel](../assets-panel-and-thumbnails/) gives it a work-area tab in the asset-editor dock island. Which panels that tab carries follows from the asset's domain, so a plant family gets a structure, appearance, and proxy workspace while a biome gets its graph.

## What opens for which subject

A plant previews its compiled renderable form on the `assetPreview` surface like any model. A biome and a map have no single renderable form, so their tabs take no preview surface at all: the preview pane says so in place and the summary panel carries the workspace.

| Subject | Live preview | Panels the domain opens |
|---|---|---|
| `.splant` plant family | Compiled family mesh | Vegetation, Plant Graph, Wind Preview, Atlas, Hierarchy, Season, Proxies |
| `.sbiome` biome | None | Vegetation, Biome Graph |
| `.svegmap` map | None | Vegetation |

Panel choice runs through the dock model rather than a render branch: the workspace calls `openPanel` and `closePanel` per domain, and the [dock](../dock-system/) collapses a leaf that ends up empty. A plant grown from a [botanical graph](../../geometry-and-assets/botanical-graph/) has no rig, so its panels open into the same leaf the skeleton tree vacates.

The workspace toolbar also scrubs a plant's authored combinations. `enter-asset-preview` reports the (variation, phenotype) pairs the family declares, and picking one binds both to the preview, because a phenotype draws a specific variation.

## What the summary reports

The Vegetation panel is the read-only account of what the cooker recorded about the subject. Four sections are common to all three domains:

| Section | Content |
|---|---|
| Validation | Issues by severity, each with its code, document path, and source selector |
| Provenance | Source name and URI, author, license, attribution text, and whether attribution is required |
| Dependencies | Exact content hash per input, its halo bits, ancestor level, and bounds |
| Latest cook | Node count, elapsed time, peak memory, cache hit rate, input and output bytes, published cells, and candidate rejections by reason |

The domain facts above them differ. A plant shows source kind, semantic parts, phenotypes, and material slots; a biome shows role, plant palette, modules, and parameter count; a map shows layer and biome-instance counts, chunk level, and world bounds in ticks. A map also lists its ordered layers with operator, coordinate space, order, lock and mute flags, dependency count, and revision.

## Plant structure

A native family's structure is a result. Parts, dimensions, spines, phenotypes, and proxies all come out of growing the graph, so the Plant Graph panel edits the graph document and the variation list and reads everything else back.

The panel's upper half is the [botanical graph](../../geometry-and-assets/botanical-graph/) on the same node canvas the material and biome editors use. One card per operator, pins typed by the domain they carry — spines, frames, shells, elements — and a right-click palette that adds any operator but the family sink, which a document already carries exactly one of. A connection whose two pins name different domains is refused at the canvas with the pin names, rather than travelling to the engine to come back as a validation code. The document stores no layout, so columns fall out of each node's longest path from a source and a dragged card keeps its place for the session.

Below it, three tabs share the selection the canvas and the tree hold:

| Tab | What it edits |
|---|---|
| Node | The selected operator's own parameters — trunk length, radius, taper, and segments; branch length and radius ratios, declination, and jitter; phyllotaxis pattern, count, span, and divergence; tropism kind, strength, stimulus, and obstacle plane; prune rule and threshold; root depth, spread, and count; shell sides and material slot; instance element, size, and roll jitter; and drawn control points. A module call shows its call site and points at the Family tab, which owns the binding |
| Structure | The grown structure as a tree — axes nested under the axis they grew from, placed elements under the axis carrying their frame — and the manual edit over whichever row is picked: move, scale, turn, trim, remove, or graft a hero mesh |
| Family | Variations, module calls, the growth summary, orphaned edits, and validation diagnostics |

Picking a structure row is the semantic selection every manual edit addresses. An edit names *that* element's identity, which is derived from ancestry, so it survives a parameter change that moves the element rather than being reassigned to whatever grew seventh. A row carrying an edit shows a dot, and the growth summary reports how many edits applied; those whose target is absent from the grown structure surface as orphans instead.

The growth summary is axes, elements, shells, grafts, vertices, triangles, parts, the grown height in metres, applied edits, and the graph's content hash. Adding a variation advances the seed so the new entry is a different individual rather than the same one twice.

A [module call](../../geometry-and-assets/botanical-graph/) is added by choosing the `.splant` preset it grows, not by dropping a bare node: the engine checks a call site and its binding against each other, so one gesture mints the call GUID, the `moduleCall` node, and the reference that says which module, which of its variations, and at what scale. Removing one takes both back out. The palette therefore offers every operator except the family sink and the module call.

Every write is `plant-graph-set` and nothing else, carrying the graph, the family's own grafts, and its module bindings — never `variations`, `phenotypes`, or the proxy lists, which would be a second truth about geometry the engine is about to re-derive. One recorded edit is one semantic operation and its inverse is the previous document replayed through that same write. A parameter scrub is one edit rather than one per keystroke: the field edits a local document immediately and the write goes out once the typing stops, so a regrow does not run per character and the socket carries one request at a time.

An imported family has no botanical graph. The panel says so in place, because that is a fact about the asset rather than an operation that failed.

## Appearance

Three panels cover what the family looks like, each anchored on a cooked artifact rather than a second reading of the rules in TypeScript.

**Atlas** shows the packed coverage atlas the family samples, read straight out of the published artifact. Re-packing the same images would produce a different arrangement and look authoritative while showing an image the plant never samples. The mip level scrubs because the chain is coverage-preserving, and the level that lost alpha area is where distant foliage goes thin. A checkerboard sits under the image so the gutter and the cut-out alpha read as transparent, and the slot list gives each placement's rectangle.

**Season** scrubs the year in thousandths with quarter marks and picks a lifecycle from juvenile, mature, senescent, or dead. Each change asks `plant-season-phenotype` which appearance the family resolves to and binds the answer to the live preview. [Lifecycle wins over season](../../geometry-and-assets/plant-rendering/): a dead plant does not turn autumnal.

**Hierarchy** lists the cooked cut: which nodes draw triangle clusters, which draw an aggregate voxel brick, each node's primitive count and page, and the appearance error it declares. The cut selector pins auto, coarse, or fine through `set-hierarchy-cut`, so a representation comparison can move the cut while the camera holds still. Flying out to reach the aggregate would shrink the subject at the same time and conflate the two changes.

## Wind and interaction

The Wind Preview panel writes the real [wind field](../../scene-and-ecs/wind-field/) and the real interaction field, the same ones a scene uses. The preview is therefore the plant's actual response rather than a model of it.

Wind offers named conditions beside the sliders, because a stiff sapling only separates from a supple reed once the field is strong enough:

| Preset | Speed | Gust |
|---|---|---|
| Still | 0 m/s | 0 |
| Breeze | 4 m/s | 0.2 |
| Wind | 12 m/s | 0.6 |
| Gale | 22 m/s | 0.9 |

Speed scrubs to 30 m/s, gust from 0 to 1, and direction over the full circle. The four push buttons emit a 32 m impulse at the subject with an explicit direction; a directionless impulse pushes outward from its own centre, which cancels there and looks exactly like nothing happening. The field is a damped oscillator, so the plant leans and springs back over about a second.

## Collision and navigation proxies

The Proxies panel draws the family's derived [collision capsules](../../physics/vegetation-collision/) and [navigation footprints](../../scene-and-ecs/vegetation-navigation/) over the preview through the same [debug overlays](../debug-visualization/) the scene uses. The table beside the toggles is the legend: each collision proxy's shape, breakable flag, and dimensions in metres, and each navigation proxy's point count, height, and traversal cost. Proxies are derived, so nothing here is editable, and a family with nothing thick enough to collide with says so rather than showing an empty list.

## Biome graphs

The Biome Graph panel renders an authored [biome graph](../../geometry-and-assets/biome-graph-evaluation/) read-only on the shared graph canvas. Node labels come from the operator name, marked `(gpu)` when the operator has a Slang compute form or the node is not declared authoritative. Pins come from the engine's closed node schema; an operator absent from the schema falls back to the pins its own edges observe.

A compile summary sits above the canvas from `vegetation-compile-biome`: the influence halo the graph requires and the evaluator's predicted caps on candidates, accepted points, and micro samples. These are graph-local planning facts, independent of any map region.

Profile evaluation runs one real evaluation instead. It resolves the bound map's instance of this biome, preflights the map's bounds, starts the job, and polls until it settles. Each node then carries its measured row: elapsed milliseconds, output cardinality, and the domain the plan placed it on.

```text
Blue noise poisson (gpu) · 4.2 ms · 18342 out · slang-compute
```

## In the code

| What | File | Symbols |
|---|---|---|
| Workspace, domain routing, and panel gating | `editor/src/panels/AssetEditorWorkspace.tsx` | `AssetEditorWorkspace`, `vegetationType`, `summaryOnly`, `onCombination` |
| Panel bodies bound to the preview context | `editor/src/panels/assetEditorPanels.tsx` | `VegetationSummaryPanel`, `AssetPreviewPanel`, `useAssetPreview` |
| Summary sections | `editor/src/panels/VegetationAssetWorkspace.tsx` | `VegetationAssetWorkspace`, `ValidationSection`, `ProvenanceSection`, `DependenciesSection`, `CookStatisticsSection` |
| Summary formatting helpers | `editor/src/panels/vegetationAssetDetails.ts` | `describeVegetationDependency`, `formatVegetationBytes`, `formatVegetationCacheRate` |
| Panel shell, write path, and burst recording | `editor/src/panels/PlantGraphPanel/index.tsx` | `PlantGraphPanel`, `write`, `commit`, `apply`, `scrub` |
| Botanical graph on the node canvas | `editor/src/panels/PlantGraphPanel/GraphEditor.tsx` | `GraphEditor`, `CANVAS_SCHEMA`, `columns`, `edgeId` |
| Operator parameter editors | `editor/src/panels/PlantGraphPanel/OperatorInspector.tsx` | `OperatorInspector`, `ScaledField`, `VectorField` |
| Structure tree and manual edits | `editor/src/panels/PlantGraphPanel/StructureTree.tsx` | `StructureTree`, `buildStructure`, `StructureRow` |
| Canonical document edits | `editor/src/panels/PlantGraphPanel/document.ts` | `withOperator`, `withAddedNode`, `withEdge`, `withEdit`, `pinsAgree` |
| Operator vocabulary and defaults | `editor/src/panels/PlantGraphPanel/botanicalSchema.ts` | `BOTANICAL_OPERATORS`, `BOTANICAL_PALETTE`, `defaultOperator` |
| Module call sites and their bindings | `editor/src/panels/PlantGraphPanel/ModuleBindings.tsx`, `editor/src/panels/PlantGraphPanel/document.ts` | `ModuleBindings`, `withModuleCall`, `withoutModuleCall`, `withModuleBinding` |
| Coverage atlas | `editor/src/panels/PlantAtlasPanel.tsx` | `PlantAtlasPanel` |
| Cut and appearance error | `editor/src/panels/PlantHierarchyPanel.tsx` | `PlantHierarchyPanel`, `CUTS`, `errorUnits` |
| Season and lifecycle | `editor/src/panels/PlantSeasonPanel.tsx` | `PlantSeasonPanel`, `LIFECYCLES`, `MARKS` |
| Wind and interaction | `editor/src/panels/PlantWindPanel.tsx` | `PlantWindPanel`, `PRESETS`, `IMPULSE` |
| Derived proxies | `editor/src/panels/PlantProxiesPanel.tsx` | `PlantProxiesPanel` |
| Biome graph and profiling | `editor/src/panels/BiomeGraphPanel.tsx` | `BiomeGraphPanel`, `biomeGraphToFlow`, `profileEvaluation` |
| Panel registration and island membership | `editor/src/components/dock/panelRegistry.tsx`, `editor/src/state/dockLayout.ts` | `ASSET_EDITOR_PANEL_REGISTRY`, `ASSET_EDITOR_PANEL_IDS`, `DEFAULT_LEAF` |
| Typed commands | `editor/src/control/client/vegetation.ts` | `plantGraphSet`, `plantAtlas`, `plantHierarchy`, `plantSeasonPhenotype`, `plantProxies`, `vegetationCompileBiome` |

## Related

- [Asset editor](../asset-editor/) — the tab, the preview view, and the surface these panels share
- [Vegetation mode](../vegetation-mode/) — paint the world with the families authored here
- [Botanical graph](../../geometry-and-assets/botanical-graph/) — the type system, growth, and manual-edit layer behind the structure panel
- [Biome graph evaluation](../../geometry-and-assets/biome-graph-evaluation/) — typed compilation, spatial evaluation, and node provenance
- [Plant rendering](../../geometry-and-assets/plant-rendering/) — family meshes, atlases, and seasonal appearance
- [Virtual geometry](../../geometry-and-assets/virtual-geometry/) — the cluster hierarchy and cut the Hierarchy panel pins
- [Vegetation assets](../../geometry-and-assets/vegetation-assets/) — the three authored formats and their catalog behaviour
