+++
title = 'Virtual geometry'
weight = 15
+++

# Virtual geometry

Virtual geometry stores renderable detail as a device-independent hierarchy of triangle clusters,
aggregate voxel nodes, assembly parts, and content-addressed pages. Ordinary `.smesh` assets and
plant families use the same hierarchy format and cooker. A view chooses a cut from projected
appearance error and residency.

## Portable hierarchy

The geometry cooker clusters normalized source triangles with portable limits of 64 vertices and 124
triangles. Each cluster stores compact local and source vertex indices, cluster-relative quantized
positions, octahedral normals and tangents, material class, conservative static and deformed bounds,
a normal cone, deformation influence, page ownership, and child/parent error.

Vertex-cache optimization, meshlet construction, and border-aware simplification follow the algorithms documented by
[meshoptimizer](https://meshoptimizer.org/); Anima uses the safe Rust `optimesh` implementation for
the cooker. Format adapters turn an ordinary `Mesh` or a normalized plant family into
`PortableHierarchyInput`; clustering, simplification, paging, codecs, and validation remain in
`saffron-geometry`.

The hierarchy branches narrowly. Leaf clusters group into sibling sets of at most
`PORTABLE_HIERARCHY_MAX_CHILDREN`, each set collapses into one parent, and the collapse repeats
until a submesh has a single root; submeshes and prototypes group the same way beneath the family
root. Contiguous solid geometry collapses a group through a border-locked simplification to a
quarter of its triangles, so each level is a real intermediate detail step the cut can stop at, and
a node's whole child set fits on the traversal's fixed page stack no matter how large the model is.
Repeated plant parts remain one prototype plus assembly transforms instead of expanding every leaf
or branch into the cooked geometry. Disconnected fine foliage collapses a group into an aggregate
voxel-brick node instead, containing occupancy, coverage density, albedo, roughness, transmission,
thickness, and normal moments.

Appearance error is a tuple rather than geometric distance alone:

```text
(silhouette, coverage, transmission, material variation, normal distribution)
```

Silhouette is a Q15.16 distance in the prototype's own local metres, measured against the group
being collapsed rather than against the whole model, and the levels' errors add downward from the
root — so a large model declares a large error and a small part of it declares its own. Parent error
bounds its descendants. A cut can therefore mix triangle and aggregate nodes while retaining a
drawable coarse root. Page dependencies are parent-first, and a parent remains usable until all
selected children and their dependencies are resident. A page's transition error is the error of the
representation drawn in its place — its parent node's — because that is what resolving the page
removes, and it is what streaming demand is priced from. The portable aggregate output is indexed
geometry, so it does not require Vulkan sparse residency.

An ordinary `.smesh` stores the five-section hierarchy in a required envelope after its conditioning
data. Mesh upload validates this envelope and carries its pages on `GpuMesh`; the GPU-scene mirror
publishes them into the global page arena the visibility traversal draws from, so rendering consumes
one import-time cook.

The device-free reference evaluator orthographically renders finest triangle descendants and their
aggregate voxel parent over six deterministic view/light fixtures. It measures silhouette distance,
coverage, transmission, material response, and normal moments against the node's declared
`AppearanceError`. The plant cook runs it on every family and widens any declared error the
measurement exceeds — only ever upward, since a measured error below the estimate means the estimate
was conservative and narrowing would trust six fixtures to have found the worst view.

The measurement also covers what aggregating takes away. A triangle cut swings each assembly use
about its pivot and shimmers the leaves; an aggregate brick has no parts, so it keeps only the
whole-plant sway. The evaluator therefore renders the triangle side displaced by the modes'
saturated amplitude — across the view, the direction that moves a silhouette most — against the
undisplaced aggregate, and the difference widens the declared error like any other. Both amplitudes
saturate before the authored response scales them, so the worst case is a property of the family
rather than of any particular gust, and the bound is exact at cook time. The effect at runtime is
that the cut selector reaches for an aggregate only once the motion it drops projects to less than
its error threshold: distant vegetation keeps moving, and stops moving only where the loss is
invisible.

## Compiled plant artifact

`.splantc` is the immutable render and runtime artifact for one `.splant` family. Its strict table of
contents has fifteen required sections:

| Section | Contents |
|---|---|
| Source normalization | Canonical source observations and compile contract |
| Part table | Semantic prototypes and repeated-part transforms |
| Geometry | Normalized family geometry |
| Materials and coverage | Material closure and canonical coverage metadata |
| Skeleton and weights | Structural hierarchy and skin influence |
| Phenotypes | Family variation and appearance states |
| Collision | Derived collision inputs |
| Navigation | Derived obstacle and cost inputs |
| Provenance | Source and license identity |
| Triangle hierarchy | Optimized clusters, hierarchy, bounds, cones, and error |
| Voxel hierarchy | Aggregate bricks, moments, and portable indexed surfaces |
| Deformation | Structural influence and swept-bound metadata |
| Page directory | Dependencies, parent order, roots, and transition metadata |
| Ray tracing | Geometry and canonical coverage derivation inputs |
| Validation | Accepted diagnostics and cook statistics |

Each section carries a semantic version, codec, alignment, uncompressed and stored lengths, and a
content hash. The artifact header binds the family, cook key, platform profile, source hashes, and
complete payload hash. Decoding rejects a missing section, an invalid dependency, a non-drawable
root, a malformed range, or a mismatched hash before the artifact can enter the store.

## Global GPU data

Device-global vertex, index, cluster, assembly-part, aggregate-voxel, and page arenas let unrelated
geometry share indexed indirect draws without CPU buffer rebinding. Immutable prototype, geometry,
material, texture, coverage, skeleton, and page tables point into those arenas. Passes bind the
global page arena as the index buffer and pull vertices through buffer device addresses — the
übershader's `vertexMainExecutor` entry.

Every table handle is an index and generation. A table slot begins with its live generation, so a
consumer can reject a stale handle. Removing a record or arena range retires it across every
frame-in-flight slot. Its index or bytes become reusable only after all relevant fences have
completed. Arena growth copies the live prefix into a larger allocation and retains the old buffer
for those same fences, keeping addresses stable for work already submitted.

One persistently mapped upload buffer belongs to each frame slot. The renderer resets a slot only
after its fence signals, stages aligned POD records into it, and declares transfer writes through the
render graph. Immutable records are replaced through new or generation-safe slots rather than
mutated underneath submitted work.

## Draw records and bins

A semantic draw record names geometry, material, instance, deformation, cluster, representation,
source generation, hierarchy state, and temporal transition. Visibility produces this record once;
binning kernels scatter each record into its bucket's `VkDrawIndexedIndirectCommand` slice, and
passes replay the slices with counted indirect draws.

The fixed PSO bin packs typed dimensions for representation, coverage classification, sidedness,
surface model, transparency, deformation, and pass. Texture identity and geometry addresses stay in
tables, outside the PSO key. This bounds command bins while preserving ordinary opaque, masked,
translucent, and thin-sheet materials.

## In the code

| What | File | Symbols |
|---|---|---|
| Hierarchy cooking and strict codecs | `geometry/src/virtual_hierarchy.rs` | `PortableHierarchyInput`, `cook_portable_virtual_hierarchy`, `decode_portable_virtual_hierarchy_sections` |
| Ordinary mesh artifact | `geometry/src/smesh.rs` | `save_mesh_to_buffer`, `load_mesh_hierarchy_from_bytes` |
| Plant-family adapter | `vegetation/src/virtual_hierarchy.rs` | `plant_hierarchy_input`, `plant_hierarchy_material` |
| Device-free comparison | `geometry/src/hierarchy_reference.rs` | `compare_triangle_voxel_transitions`, `TriangleVoxelReferenceComparison` |
| Plant artifact assembly | `assets/src/plant_cook/publish.rs` | `build_plant_sections`, `validate_complete_plant_artifact` |
| Artifact store validation | `assets/src/vegetation_store.rs` | `validate_plant_artifact` |
| Global arenas and handles | `rendering/src/global_gpu_data.rs` | `GlobalGpuData`, `GlobalGpuArena`, `GpuHandle`, `ResidentGpuTable` |
| Upload and draw ABI | `rendering/src/global_gpu_data.rs` | `FrameUploadRing`, `GpuBufferUpload`, `GpuDrawRecord`, `GpuPsoBin` |

## Related

- [Vegetation cooking](../vegetation-cooking/) — staged publication and complete artifact closure
- [Vegetation assets](../vegetation-assets/) — the authored `.splant` owner
- [Native materials](../../materials-and-pipelines/native-materials/) — thin-sheet response and coverage
- [Barrier derivation](../../frame-and-render-graph/usage-and-barrier-derivation/) — transfer, compute, indirect, and arena-read synchronization
