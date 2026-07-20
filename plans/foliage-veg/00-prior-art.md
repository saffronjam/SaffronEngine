# Prior art and chosen approach

**Status:** reference

This is the research record behind `plans/foliage-veg/`. It separates verified source facts from
Anima's design decisions. Experimental systems and vendor measurements are evidence, not promises or
acceptance criteria. The research surveyed current official documentation and primary papers across
Unreal Engine 5.8, Unity 6, Godot 4, O3DE, SpeedTree, Houdini, OpenUSD, production GDC talks, Vulkan,
and graphics/ecosystem research.

## Major-engine authoring

### Unreal Engine 5.8

- Foliage Mode exposes palettes plus paint, erase, select/lasso, reapply, single, and fill tools,
  target-surface filters, brush density, random scale, and normal alignment. Static Mesh Foliage is
  hardware-instanced; Actor Foliage carries ordinary actor cost. Its documented cluster behavior
  culls and changes LOD in groups. [Foliage Mode](https://dev.epicgames.com/documentation/en-us/unreal-engine/foliage-mode-in-unreal-engine)
- Landscape Grass couples generation to Landscape material layers. This validates a convenient
  terrain workflow but also demonstrates the fragmentation Anima must avoid.
  [Grass Quick Start](https://dev.epicgames.com/documentation/en-us/unreal-engine/grass-quick-start-in-unreal-engine)
- PCG moves typed spatial and point data through graphs, evaluates in editor or runtime, and exposes
  node inspection/debug rendering. Partitioned and hierarchical generation uses different cell sizes
  for different scales, caches coarse work, and supports runtime generation/cleanup around sources.
  [PCG overview](https://dev.epicgames.com/documentation/en-us/unreal-engine/procedural-content-generation-overview),
  [generation modes](https://dev.epicgames.com/documentation/en-us/unreal-engine/using-pcg-generation-modes-in-unreal-engine)
- PCG Biome Core supports data-defined biomes, volume/spline/texture boundaries, blending,
  priorities, exclusions, recursive children, filtering, and hierarchical generation. It is an
  experimental sample/plugin, not a stable contract.
  [Biome Core](https://dev.epicgames.com/documentation/en-us/unreal-engine/procedural-content-generation-pcg-biome-core-and-sample-plugins-reference-guide-in-unreal-engine)
- GPU PCG can keep compatible graph groups on the GPU, but Epic documents CPU/GPU transfer cost and
  substantial semantic omissions for procedurally instanced GPU meshes, including persistence,
  collision, navigation, ray tracing, distance-field lighting, and HLOD. That makes GPU-only output
  unsuitable as plant authority.
  [GPU PCG](https://dev.epicgames.com/documentation/en-us/unreal-engine/using-pcg-with-gpu-processing-in-unreal-engine)
- The experimental Procedural Vegetation Editor models plant structure through branch generations,
  hormones, tropisms, gravity, pruning, bones, materials, growth, and foliage grafting, then outputs
  static or skeletal/Nanite-ready meshes. This validates native botanical authoring as a current
  high-end destination without making its unstable API an Anima dependency.
  [PVE](https://dev.epicgames.com/documentation/en-us/unreal-engine/procedural-vegetation-editor-pve-in-unreal-engine),
  [UE 5.8 release notes](https://dev.epicgames.com/documentation/en-us/unreal-engine/unreal-engine-5-8-release-notes)

**Chosen lesson:** do not reproduce Unreal's separate Foliage, Landscape Grass, Procedural Foliage,
PCG, and PVE stores. Anima has one typed point/layer/evaluator model and one normalized plant-family
output.

### Unity 6 and Entities

- Unity Terrain separates Paint Trees from Paint Details. Trees provide prototypes, mass/brush
  placement, randomized transform/color, SpeedTree integration, LOD, and terrain-level collision.
  Details provide texture cards, meshes, and instanced meshes with seeded density/jitter/alignment.
  [Trees](https://docs.unity3d.com/6000.0/Documentation/Manual/terrain-Trees.html),
  [grass/details](https://docs.unity3d.com/6000.0/Documentation/Manual/terrain-Grass.html),
  [tree collision](https://docs.unity3d.com/6000.0/Documentation/Manual/terrain-Tree-Colliders.html)
- Entities Graphics gathers ECS render data; runtime creation of the full rendering component set is
  not an authoring model and is not a reason to represent every blade as an entity.
  [Entities Graphics](https://docs.unity3d.com/Packages/com.unity.entities.graphics@1.4/manual/index.html),
  [runtime creation](https://docs.unity3d.com/Packages/com.unity.entities.graphics@1.4/manual/runtime-entity-creation.html)
- Entities scene sections stream asynchronously with explicit reference restrictions.
  [Scene streaming](https://docs.unity3d.com/Packages/com.unity.entities@1.4/manual/streaming-scenes.html)

**Chosen lesson:** plant authoring cannot be a heightfield appendage, and dense decorative
vegetation cannot be one ECS entity per instance. Streaming ownership and stable cross-cell identity
must be explicit.

### O3DE

- O3DE's Vegetation Gem composes descriptor providers/selectors, areas, filters, modifiers,
  blockers, priorities, tagged surfaces, and reusable gradients. Filters include altitude, slope,
  distance, distributions, shapes, and surface masks.
  [Vegetation Gem](https://www.docs.o3de.org/docs/user-guide/gems/reference/environment/vegetation/),
  [filters](https://www.docs.o3de.org/docs/user-guide/components/reference/vegetation-filters/)
- Surface Data and Gradient Signal expose semantic tags and continuous fields; Landscape Canvas
  gathers them into a graph.
  [Landscape Canvas](https://www.docs.o3de.org/docs/user-guide/gems/reference/environment/landscape-canvas/)
- The runtime uses a camera-centered sector grid, intersects candidate points with overlapping
  surfaces, resolves prioritized layers, and asynchronously rebuilds dirty sectors.
  [Vegetation Area System](https://www.docs.o3de.org/docs/user-guide/components/reference/vegetation/vegetation-area-system/)

**Chosen lesson:** O3DE supplies the strongest field/tag/filter vocabulary, but Anima compiles it
into typed graph assets instead of requiring scenes full of helper components. One regular sample
lattice is not enough; samplers include blue-noise, stratified, cluster/colony, spline, recursive,
and explicit-anchor families.

### Godot 4 and Terrain3D

- Godot MultiMesh can render very high instance counts, but all instances in one MultiMesh share a
  visibility decision; the documentation recommends spatial subdivision.
  [MultiMesh optimization](https://docs.godotengine.org/en/stable/tutorials/performance/using_multimesh.html)
- Terrain3D is a useful community case study: it subdivides MultiMesh placement into cells, separates
  stored/manual placement from ephemeral GPU generation, and explicitly documents missing
  per-instance culling/collision in that path.
  [Terrain3D instancer](https://terrain3d.readthedocs.io/en/latest/docs/instancer.html)

**Chosen lesson:** cells are useful storage/bounds units but cannot impose all-or-nothing visibility
or LOD. Manual and procedural placement share a schema rather than becoming unrelated systems.

### SpeedTree and Houdini

- SpeedTree combines procedural generators with nondestructive manual node offsets and recommends
  retaining procedural structure as long as possible. It covers wind, LOD, seasons, growth,
  hand-drawing, trimming, pruning, vines, materials, and export.
  [Modeling approach](https://docs.unity3d.com/speedtree-modeler/manual/modeling-approach.html),
  [documentation map](https://docs.unity3d.com/speedtree-modeler/manual/doc-map.html)
- Houdini HeightField Scatter supports deterministic seeds, density/exact-count modes, relaxation,
  and safety caps; point attributes carry transforms, color, temperature, and wind direction.
  [HeightField Scatter](https://www.sidefx.com/docs/houdini/nodes/sop/heightfield_scatter-.html),
  [scatter attributes](https://www.sidefx.com/docs/houdini/heightfields/scatterattribs.html)
- Houdini biome tools describe species preferences, spacing, variations, growth stages, and dead
  variants. Solaris treats point attributes/prototype indices as first-class and supports sparse
  edits and nested instancing.
  [Biome Plant Define](https://www.sidefx.com/docs/houdini/nodes/sop/labs--biome_plant_define-1.0.html),
  [Biome Plant Scatter](https://www.sidefx.com/docs/houdini/nodes/sop/labs--biome_plant_scatter-1.2.html),
  [point instancing](https://www.sidefx.com/docs/houdini/solaris/support/point_instancing.html)
- Houdini Engine/PDG distinguishes temporary cooking from permanent baking and exposes provenance
  and ownership pitfalls when regeneration cannot clean up pre-existing data.
  [PCG integration](https://www.sidefx.com/docs/houdini/unreal/pcg/overview.html),
  [PDG](https://www.sidefx.com/docs/houdini/tops/intro.html)

**Chosen lesson:** procedural plant structure and manual semantic offsets coexist in `.splant`;
typed point attributes and provenance are the shared interchange; generated outputs have exact
ownership and cleanup. Direct proprietary SpeedTree formats remain conditional on licensing; the
committed path accepts standard exports originating from SpeedTree.

## Point instancing and identity

OpenUSD `PointInstancer` is designed for enormous repeated populations and separates positions,
orientations, scales, prototype indices, stable IDs, and sparse inactive/invisible masks. Its
list-editable `inactiveIds` illustrates why sparse stable-ID overrides survive upstream changes
better than buffer-index edits.
[UsdGeomPointInstancer](https://openusd.org/24.08/api/class_usd_geom_point_instancer.html)

**Chosen lesson:** Anima uses typed columnar points and opaque stable `PlantId`s. GPU layouts compile
from that schema. GPU slots, render handles, and accepted-array positions are never identity.

## Deterministic placement and ecosystem rules

- Random123/Philox-style counter generators are stateless functions of a key and counter and are
  designed for independent parallel streams.
  [Random123](https://random123.com/),
  [SC11 paper](https://www.thesalmons.org/john/random123/papers/random123sc11.pdf)
- Bridson's grid-accelerated Poisson-disk algorithm produces blue-noise separation in expected
  linear work for fixed attempts; weighted sample elimination supports progressive blue-noise
  subsets and arbitrary domains.
  [Bridson](https://www.cs.ubc.ca/~rbridson/docs/bridson-siggraph07-poissondisk.pdf),
  [Yuksel](https://www.cemyuksel.com/research/sampleelimination/)
- Deussen et al. combine manual placement and ecosystem simulation; Lane and Prusinkiewicz discuss
  individual and density-based ecosystem models, clustering, and succession.
  [Deussen et al.](https://graphics.uni-konstanz.de/publikationen/Deussen1998RealisticModelingRendering/index.html),
  [Lane and Prusinkiewicz](https://algorithmicbotany.org/papers/eco.gi2002.html)
- A 2024 Eurographics paper uses data-oriented layout, spatial hashing, and parallelism for
  competition among hundreds of thousands of trees.
  [Real-time forestry simulation](https://diglib.eg.org/items/6c6f07ca-9eb8-4450-ab71-5765644452b4)

**Chosen lesson:** stable node GUIDs and semantic revisions domain-separate random channels; the
whole graph hash invalidates cache but does not reshuffle unrelated streams. Macro decisions use
canonical numeric semantics and deterministic owner/halo rules. Initial behavior is called
ecological rule evaluation unless a biological model is separately validated. Simulation uses
fixed-tick, double-buffered cross-cell state and proven catch-up equivalence.

## Production vegetation rendering

### Unreal Nanite Foliage 5.8

Epic's experimental Nanite Foliage combines:

- Assemblies: repeated branch/frond/leaf parts remain micro-instances rather than expanded geometry.
- Voxels: near-pixel aggregate clusters preserve volume/material/animation and replace triangles
  when their representation error is lower.
- Skinning: hierarchical bones replace arbitrary WPO, enabling tighter bounds and fixed raster work.

Epic explicitly describes the feature as Experimental. Its documentation also explains why dense
alpha cards hurt simplification/culling/overdraw, why WPO inflates conservative bounds, how triangle
and voxel clusters can coexist in a hierarchy cut, and how voxel shading retains normal
distributions.
[Nanite Foliage](https://dev.epicgames.com/documentation/en-us/unreal-engine/nanite-foliage),
[Nanite Assemblies](https://dev.epicgames.com/documentation/en-us/unreal-engine/nanite-assemblies),
[Nanite virtual geometry](https://dev.epicgames.com/documentation/en-US/unreal-engine/nanite-virtualized-geometry-in-unreal-engine),
[Nanite SIGGRAPH 2021](https://advances.realtimerendering.com/s2021/Karis_Nanite_SIGGRAPH_Advances_2021_final.pdf)

**Chosen lesson:** build an independently specified assembly + triangle/aggregate-voxel hierarchy,
not a clone of unstable Epic APIs. Representation error includes appearance, not only geometric
distance. Modeled leaf/blade silhouettes are primary; alpha remains only for irreducible micro-detail.

### GPU procedural generation

- Sucker Punch generated individual Ghost of Tsushima grass blades on the GPU and used a shared
  wind/interaction field with damped recovery.
  [Procedural grass](https://gdcvault.com/play/1027033/),
  [visual-effects article](https://blog.playstation.com/2021/01/12/how-stunning-visual-effects-bring-ghost-of-tsushima-to-life/)
- Its world-building talk describes functional placement interpreted as GPU bytecode for millions
  of instances.
  [Samurai Landscapes](https://www.gdcvault.com/play/1027352/Samurai-Landscapes-Building-and-Rendering)
- A 2025 HPG paper generates and renders procedural trees directly through GPU work graphs, with
  continuous frame-specific detail, animation, pruning, and seasons from compact definitions.
  [Real-Time GPU Tree Generation](https://diglib.eg.org/items/93fc78c0-71fa-4511-8564-a7e5268bf27a)

**Chosen lesson:** preserve a procedural plant/placement IR that can map to future work graphs, but
do not make current assets depend on provisional `VK_AMDX_shader_enqueue` or vendor execution.

## Vulkan execution and portability

- `VK_EXT_mesh_shader` provides task/mesh work generation and dynamic primitive counts, but exposes
  device limits and preferred granularities that applications must query.
  [Mesh shader proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_mesh_shader.html)
- Buffer device address, descriptor indexing, multi-draw indirect, and draw-indirect-count are the
  portable foundation for GPU-driven rendering.
  [BDA](https://docs.vulkan.org/samples/latest/samples/extensions/buffer_device_address/README.html),
  [descriptor indexing](https://docs.vulkan.org/samples/latest/samples/extensions/descriptor_indexing/README.html),
  [multi-draw indirect](https://docs.vulkan.org/samples/latest/samples/performance/multi_draw_indirect/README.html),
  [draw indirect count](https://docs.vulkan.org/guide/latest/extensions/VK_KHR_draw_indirect_count.html)
- MoltenVK's current supported-extension list includes the necessary BDA/descriptor/indirect
  foundations but not `VK_EXT_mesh_shader`; extension names still do not replace feature-bit/limit
  queries.
  [MoltenVK runtime guide](https://github.com/KhronosGroup/MoltenVK/blob/main/Docs/MoltenVK_Runtime_UserGuide.md)
- `VK_EXT_device_generated_commands` targets device-driven traversal, LOD, culling, and work
  generation. `VK_AMDX_shader_enqueue` remains experimental.
  [Device generated commands](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_device_generated_commands.html),
  [AMDX execution graphs](https://docs.vulkan.org/features/latest/features/proposals/VK_AMDX_shader_enqueue.html)
- Vulkan tile-rendering guidance emphasizes attachment traffic and discard/coverage costs.
  [Tile best practices](https://docs.vulkan.org/guide/latest/tile_based_rendering_best_practices.html)

**Chosen lesson:** the required executor is compute visibility plus indexed MDI over global
geometry/material/draw tables. Visibility emits semantic cluster records; indexed and mesh-task
executors derive different command bytes from those records. All content and quality work without
mesh shaders. Async compute changes render-graph scheduling only, never algorithms.

## Coverage, leaf shading, and temporal stability

- Hashed alpha testing avoids scale-dependent disappearance by replacing a fixed threshold with a
  spatially stable hash; improved variants refine stability/quality.
  [Hashed alpha testing](https://research.nvidia.com/sites/default/files/pubs/2017-02_Hashed-Alpha-Testing/Wyman2017Hashed.pdf),
  [improved alpha hashing](https://research.nvidia.com/labs/rtr/publication/wyman2019improved/)
- Physically based leaf work models reflection plus transmission/subsurface effects rather than
  treating a leaf as ordinary opaque PBR or generic alpha blend.
  [Physically based leaf translucency](https://diglib.eg.org/items/e7e2b825-64f0-4059-adf9-1ad4a2f089aa/full)

**Chosen lesson:** `.smat` gains one thin-sheet surface model with thickness/absorption/front-back
response and energy conservation. Coverage-preserving mips, A2C under MSAA, stable hashed coverage
under TAA, depth/shadow/picking agreement, and OMM derivation share one canonical coverage source.
Aggregate voxels retain the moments needed to reproduce the thin-sheet response.

## Wind and interaction

- GPU Gems describes procedural hierarchical tree wind; modal and physically guided research
  derives branch response from structure and frequency-space wind.
  [GPU Gems 3](https://developer.nvidia.com/gpugems/gpugems3/part-i-geometry/chapter-6-gpu-generated-procedural-wind-animations-trees),
  [modal animation](https://diglib.eg.org/items/5125d70b-aecc-4b9d-bc88-a716125e96bb),
  [physically guided tree animation](https://diglib.eg.org/items/e54ecbf5-83fe-4f87-8168-a2a23eb855b0)
- God of War's production system includes spatially varying wind, vegetation motion, interaction,
  and settling.
  [Interactive Wind and Vegetation](https://gdcvault.com/play/1026036/Interactive-Wind-and-Vegetation-in)

**Chosen lesson:** Environment owns one sampled wind field; plant assets own structural response.
Compute produces current/previous transforms and swept bounds once. Interaction emitters feed the
same damped field/solver. Far aggregates reduce deformation state by error bounds rather than
stopping wind.

## Shadows, GI, and ray tracing

- Virtual Shadow Maps use demand-paged shadow data and are designed for high-detail dynamic worlds.
  [Epic VSM](https://dev.epicgames.com/documentation/en-us/unreal-engine/virtual-shadow-maps-in-unreal-engine)
- Opacity micromaps encode alpha coverage for RT traversal. NVIDIA's Indiana Jones case study reports
  large scene-specific vegetation wins from OMM and dynamic-BLAS compaction, but those measurements
  are not Anima budgets.
  [KHR opacity micromap](https://docs.vulkan.org/features/latest/features/proposals/VK_KHR_opacity_micromap.html),
  [Indiana Jones case study](https://developer.nvidia.com/blog/path-tracing-optimizations-in-indiana-jones-opacity-micromaps-and-compaction-of-dynamic-blass/)
- NVIDIA cluster and partitioned acceleration structures are optional high-end capabilities.
  [Cluster AS](https://docs.vulkan.org/features/latest/features/proposals/VK_NV_cluster_acceleration_structure.html)

**Chosen lesson:** use a portable physical VSM atlas and page table, not Vulkan sparse residency.
The same hierarchy, coverage, deformation, and swept bounds drive main and shadow views. KHR RT
materializes selected assemblies for BLAS work or uses cooked/procedural representations; standard
KHR RT is not assumed to provide nested part instancing. Any-hit is baseline-correct, OMM optional.

## Streaming, physics, navigation, persistence, and networking

- UE World Partition and PCG builders show cell-addressed streaming/build work; PDG shows
  dependency-addressed incremental cook.
  [World Partition](https://dev.epicgames.com/documentation/unreal-engine/world-partition-in-unreal-engine),
  [builder commandlets](https://dev.epicgames.com/documentation/unreal-engine/world-partition-builder-commandlet-reference)
- Jolt documents batching body insertion/removal for streaming workloads.
  [Jolt 5.3 architecture](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/)
- Unreal navigation guidance favors simplified collision, exclusion of irrelevant small objects,
  tile-local rebuilds, and streamable world-partition nav data.
  [Navigation optimization](https://dev.epicgames.com/documentation/en-us/unreal-engine/optimizing-navigation-mesh-generation-speed-in-unreal-engine),
  [World Partition navmesh](https://dev.epicgames.com/documentation/unreal-engine/world-partitioned-navigation-mesh)
- Iris and Replication Graph use spatial filtering/prioritization rather than broadcasting every
  world object.
  [Iris filtering](https://dev.epicgames.com/documentation/unreal-engine/iris-filtering-in-unreal-engine),
  [Replication Graph](https://dev.epicgames.com/documentation/unreal-engine/replication-graph-in-unreal-engine)

**Chosen lesson:** runtime residency is facet-based. Macro CPU state remains authoritative;
collision-relevant plants gain batched Jolt proxies, and a sparse subset promotes to entities.
Navigation receives typed obstacle/cost contributions, not grass bodies. Saves bind to an exact base
manifest and store compact state snapshots plus mutation tails. Future networking reuses cell keys,
manifest binding, snapshots, and idempotent mutations while owning transport and authority elsewhere.

## Rejected destinations

- CPU cell culling followed by a later GPU rewrite.
- Per-frame CPU `Vec<DrawItem>` construction for foliage.
- A material node that performs foliage world-position-offset wind.
- Classic discrete mesh LOD chains plus octahedral/crossed-card impostors.
- Alpha-card forests as the primary leaf representation.
- One hecs entity or Jolt body per decorative plant.
- A camera-centered rendering grid as plant authority.
- Terrain-exclusive grass, a GPU-only grass store, or a separate painted-foliage store.
- Seed-only persistence or replication.
- A monolithic `.svegmap`, all-or-nothing `.svegcell`, or mutable generated cache.
- One append-only physical log shared by editor undo, saves, and networking.
- Shader floating point assumed deterministic across vendors.
- Vulkan sparse residency as the virtual-shadow portability floor.
- Mesh shaders, opacity micromaps, vendor RT, device-generated commands, or work graphs as baseline
  correctness requirements.

