+++
title = 'Virtual geometry'
weight = 15
+++

# Virtual geometry

Virtual geometry stores renderable detail as a device-independent hierarchy of triangle clusters,
aggregate voxel nodes, assembly parts, and content-addressed pages. A view chooses a hierarchy cut
from projected appearance error and residency. It never chooses a separately authored foliage LOD,
billboard, or platform-specific species representation.

## Portable hierarchy

The plant cooker clusters normalized source triangles with portable limits of 64 vertices and 124
triangles. Each cluster stores compact local indices, cluster-relative quantized positions,
octahedral normals and tangents, material class, conservative static and deformed bounds, a normal
cone, deformation influence, page ownership, and child/parent error. Vertex-cache optimization,
meshlet construction, and border-aware simplification follow the algorithms documented by
[meshoptimizer](https://meshoptimizer.org/); Anima uses the safe Rust `optimesh` implementation for
the cooker.

Contiguous solid geometry simplifies through border-locked hierarchy groups. Repeated plant parts
remain one prototype plus assembly transforms instead of expanding every leaf or branch into the
cooked geometry. Disconnected fine foliage also receives aggregate voxel-brick nodes containing
occupancy, coverage density, albedo, roughness, transmission, thickness, and normal moments.

Appearance error is a tuple rather than geometric distance alone:

```text
(silhouette, coverage, transmission, material variation, normal distribution)
```

Parent error bounds its descendants. A cut can therefore mix triangle and aggregate nodes while
retaining a drawable coarse root. Page dependencies are parent-first, and a parent remains usable
until all selected children and their dependencies are resident. The portable aggregate output is
indexed geometry, so it does not require mesh shaders or Vulkan sparse residency.

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
material, texture, coverage, skeleton, and page tables point into those arenas. Portable indexed
drawing uses global offsets or buffer device addresses; a mesh-shader executor reads the same
semantic records when its individual feature bits and limits qualify.

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
source generation, hierarchy state, and temporal transition. Visibility produces this record once.
Indexed indirect and mesh-task executors derive their command formats from it rather than defining
different content paths.

The fixed PSO bin packs typed dimensions for representation, coverage classification, sidedness,
surface model, transparency, deformation, and pass. Texture identity and geometry addresses stay in
tables, outside the PSO key. This bounds command bins while preserving ordinary opaque, masked,
translucent, and thin-sheet materials.

## In the code

| What | File | Symbols |
|---|---|---|
| Hierarchy cooking and strict codecs | `vegetation/src/virtual_hierarchy.rs` | `cook_portable_virtual_hierarchy`, `validate_portable_virtual_hierarchy`, `decode_portable_virtual_hierarchy_sections` |
| Plant artifact assembly | `assets/src/plant_cook.rs` | `build_plant_sections`, `validate_complete_plant_artifact` |
| Artifact store validation | `assets/src/vegetation_store.rs` | `validate_plant_artifact` |
| Global arenas and handles | `rendering/src/global_gpu_data.rs` | `GlobalGpuData`, `GlobalGpuArena`, `GpuHandle`, `ResidentGpuTable` |
| Upload and draw ABI | `rendering/src/global_gpu_data.rs` | `FrameUploadRing`, `GpuBufferUpload`, `GpuDrawRecord`, `GpuPsoBin` |

## Related

- [Vegetation cooking](../vegetation-cooking/) — staged publication and complete artifact closure
- [Vegetation assets](../vegetation-assets/) — the authored `.splant` owner
- [Native materials](../../materials-and-pipelines/native-materials/) — thin-sheet response and coverage
- [Barrier derivation](../../frame-and-render-graph/usage-and-barrier-derivation/) — transfer, compute, indirect, and arena-read synchronization
