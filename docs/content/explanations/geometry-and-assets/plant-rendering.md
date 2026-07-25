+++
title = 'Plant rendering'
weight = 16
+++

# Plant rendering

A cooked plant family renders through the same GPU-scene prototype path as every mesh:
one flattened geometry, one prototype record, streamed hierarchy pages, and GPU-binned
executor draws. What makes a family different is assembly — the cooked hierarchy places
each source prototype through *uses* (per-part local transforms), and the renderer
expands those uses on the GPU instead of duplicating geometry.

## From artifact to family mesh

`AssetServer::load_plant_family` reads the family's validated `.splantc` by exact
content identity from the vegetation artifact store and decodes three groups of
sections: the portable triangle/voxel hierarchy (pages, prototypes, uses), the
geometry rows (one quantized mesh per prototype), and the materials table (slot order
plus each pinned `.smat` document). Each geometry row is verified against its
hierarchy prototype: source identity, selector hash, vertex and submesh counts. The
rows concatenate in prototype-id order into one flat vertex/index stream.

The upload packs the assembly-part table into the geometry's parts arena range: a
per-prototype record `{first_use, use_count, vertex_base}` (the prototype's base
vertex within the flattened stream), then every use record (rows 0–2 of the family-
local transform plus the placed prototype). The prototype count rides the geometry
record's reserved word so shaders can split the two tables. A plain single-prototype
mesh keeps its parts range empty and takes none of the assembly paths.

The loaded family registers under its family id in the shared mesh and page-payload
caches, so the mirror resolves it like any mesh; its pinned material documents
register under their material ids for interning. An assembly carries no merged BLAS —
its ray-traced shape is per-use instancing, not the concatenated prototype streams.

Each instance selects one authored `(variation, phenotype)` combination. The
phenotype it renders derives from typed state, never from an active mesh: a dead or
stump lifecycle takes the family's `Dead`-role phenotype, a senescent lifecycle the
`Senescent` role, and a healthy plant the first phenotype whose seasonal window
contains the calendar's phase — the cooked phenotype is the fallback throughout.
`season_phase_mille` folds the date and hemisphere into a per-mille year phase, and
seasonal roles (`Flowering`, `Fruiting`, `Senescent`) carry authored or role-default
windows in the `.splant` phenotype rows.

The resolved pair then matches the family's combination masks (exact pair match,
then the phenotype alone, then the first authored combination). A plain scene
entity referencing the family mesh selects the same way through an optional
`PlantVariant` component — the asset preview's variation scrub sets it via
`set-asset-preview-options {variation, phenotype}` — and renders the first authored
combination without one.

## The macro snapshot adapter

`GpuSceneMirror::sync_vegetation` translates the authoritative
[vegetation runtime world](../../scene-and-ecs/vegetation-state/) into persistent-scene
instances. Cells diff purely by published generation id: an unchanged cell is skipped,
and a republished cell (any load, unload, or state mutation) removes and recreates its
instances inside one sync pass, so a plant never has two visible representations.

A seasonal phase change flips combinations in place instead: every live plant whose
resolved combination moved gets one instance update carrying the previous combination
and a flip stamp in the static payload.

During the transition window the traversal emits assembly uses of both masks — a use
only in the new mask with an incoming crossfade word, only in the old with an
outgoing one, sharing one flip id so the stochastic coverage partitions every pixel
exactly. The phase derives from the frame stamp, so the crossfade completes on its
own and the instance handle never changes.
Instances key on `(WorldCellKey, PlantId)` — the stable 128-bit identity, never a slot
index — and survive GPU-scene loss because the map rebuilds from snapshots.

Each accepted macro point becomes one instance: the family's prototype, per-slot
material overrides from the family's slot table, and a compact static transform
(`GpuSceneStaticTransform`) packing the exact cell coordinates, local position ticks,
quantized orientation, and Q15.16 scale with no CPU float conversion. Dormant seeds and
tombstoned plants are skipped. Streamed vegetation changes request a repaint, so the
reactive host keeps painting until the temporal effects converge.

```sh
sa -o json gpu-scene-stats   # instances counts entities + plants; records > 0 once resident
sa -o json vegetation-runtime-cell '{"coordinates":["0","0","0"],"level":0}'
sa -o json vegetation-render-stats   # per-family/per-cell population + page faults
```

## Static transforms on the GPU

Shaders read every instance's world columns through one helper,
`gpuSceneInstanceColumns`. A dynamic record stores explicit float columns (current and
previous). A static record stores the 64-byte compact placement, decoded in the
shader: cell coordinates scale by the 64 m cell edge, ticks add the fractional metre
(1/4096 m), the quantized XYZW quaternion normalizes into rotation columns, and the
Q15.16 scale multiplies per axis.

A static point has no motion, so its previous columns equal its current ones. The
visibility cull, the traversal, the transparent sort keys, and every executor vertex
path share this decode.

The compact placement fills half the 128-byte transform payload; a plant's remaining
words carry its vegetation columns. Words 16..24 hold conservative current and
previous bounds spheres in instance-local pre-scale space — the cull composes them
instead of the prototype sphere (the `GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS` flag).
Words 24..30 hold the stable surface-attachment identity (provider, primitive,
barycentrics) when the point is surface-attached, and the instance flags carry the
two-bit interaction policy.

## Micro vegetation fields

Ground cover reconstructs from authoritative density tiles, never from stored blades.
Each resident cell's quantized micro tiles (density plus typed attribute channels) pack
into the fields arena with the same generation lifecycle as the cell's plants, and a
cell-ordered directory lists every resident tile with its per-family field instance —
a flagged identity instance the visibility cull skips.

A count → scan → scatter compute chain walks the directory each frame. The count pass
measures each tile's post-cull blade survivors (density-thresholded, distance-gated,
frustum-tested); the scan assigns every tile an exact exclusive base under the frame's
candidate and record budgets; the scatter re-derives the identical blade set and writes
each survivor's candidate and record at its exact slot. No atomics order the stream, so
record order is bitwise stable frame to frame.

Placement, height, and facing derive from a hash of the tile's reconstruction seed and
texel coordinates — camera travel changes residency, never established placement. Blade
records join the same semantic record stream the traversal emits, ahead of binning.

Each directory entry also carries the tile's predicted budget: the blade count a fully
visible frame would reconstruct, summed from the same density derivation at pack time.
`gpu-scene-stats` reports the directory total as `microPredicted`; the per-frame
generated count never exceeds it. A tile that does not fit the frame's budgets — and
every tile after it — skips whole with the pressure flag raised; density is never
thinned silently and no partial tile ever emits.

Blades draw through the ordinary indexed executor: the bin scatter points blade
commands at a shared 24-index template block in the pages arena, and the executor
vertex paths derive each template vertex procedurally from the record's candidate — a
tapered, bowed strip in world space. No blade vertex data exists anywhere.

The scatter also bakes each survivor's analytic [wind](../../scene-and-ecs/wind-field/)
bend into its candidate: the shared field sampled at the blade root for the current and
the previous frame's time, horizontal and capped by blade height. The blade builder
applies the stored bend tip-weighted with a parabolic tip drop, so every raster pass
bends the blade identically and the motion pass rebuilds the previous frame's bend for
exact blade motion vectors. Placement and the survivor count stay bend-independent, so
the count and scatter passes always agree.

```sh
sa -o json gpu-scene-stats   # visibility.microCandidates ≤ microPredicted
```

## Assembly execution

The visibility traversal walks a family's resident pages exactly like a mesh's. A node
whose subtree belongs to one prototype forks per use: it emits one draw record per use
of that prototype, scaling the node's appearance error by the use's basis scale, and
stores the use index in the record's `clusterState` word. `GPU_ASSEMBLY_NO_USE` marks
the no-assembly case — every plain mesh, and family nodes spanning prototypes.

The executor vertex paths then rebase the vertex fetch to the placed prototype's slice
(`vertex_base + pulled index`) and premultiply the use's family-local transform before
the instance transform. Memory stays flat: uses expand at traversal time, never in the
geometry or page payloads.

## In the code

| What | File | Symbols |
|---|---|---|
| Family load, decode, and flatten | `assets/src/plant_render.rs` | `load_plant_family`, `PlantFamilyRender` |
| Section decode mirrors | `assets/src/plant_cook.rs` | `decode_mesh_section`, `decode_plant_material_document` |
| Assembly-part table build | `rendering/src/upload.rs` | `assembly_from_hierarchy`, `MeshAssembly` |
| Macro snapshot adapter | `assets/src/gpu_scene_mirror.rs` | `sync_vegetation` |
| Field-tile packing + directory | `assets/src/gpu_scene_mirror.rs` | `pack_field_tiles`, `rebuild_field_directory` |
| Blade reconstruction chain | `assets/shaders/scene_micro_common.slang` | `microTexel`, `microBlade`, `microTexelSurvivors` |
| Blade template + candidates | `rendering/src/global_gpu_data.rs` | `micro_blade_template_indices`, `GpuMicroCandidate` |
| Resident-cell snapshots | `vegetation/src/runtime_world.rs` | `VegetationWorld::resident_cells` |
| Compact exact placement | `rendering/src/persistent_gpu_scene.rs` | `GpuSceneStaticTransform` |
| GPU transform decode + assembly loaders | `assets/shaders/global_gpu_data.slang` | `gpuSceneInstanceColumns`, `gpuSceneAssemblyUse` |
| Traversal use fork | `assets/shaders/scene_traversal.slang` | `emitNodeRecords`, `assemblyUseScale` |

## Related

- [Virtual geometry](../virtual-geometry/) — the portable hierarchy, pages, and cluster records
- [Vegetation cooking](../vegetation-cooking/) — how the `.splantc` and manifest are produced
- [Vegetation state](../../scene-and-ecs/vegetation-state/) — the runtime authority the adapter reads
- [Persistent GPU scene](../../frame-and-render-graph/persistent-gpu-scene/) — prototypes, instances, and deltas
- [Hierarchical visibility](../../frame-and-render-graph/hierarchical-visibility/) — the cull and traversal chain
