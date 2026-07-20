# Phase 6 — Virtual geometry and render substrate

**Status:** NOT STARTED

**Depends on:** Phases 2 and 4

This phase builds the final data and Vulkan substrate that the Phase-7 atomic renderer cutover needs.
It does not introduce an alternate runtime renderer or route any production view through partial
infrastructure. Offline cooks, focused GPU tests, and benchmark fixtures validate the new contracts
until the cutover is complete.

## Render-graph buffer and indirect primitives

- [ ] Extend `engine/crates/rendering/src/render_graph.rs` with imported and graph-owned persistent/
  transient buffer resources, byte ranges, and explicit usages for storage read/write, transfer,
  vertex/index/BDA reads, indirect command/count reads, and acceleration-structure build input.
- [ ] Derive compute-write → indirect-read, compute-write → vertex/index/BDA-read, transfer, queue
  ownership, and AS-build barriers. Validation tests cover every transition.
- [ ] Add count→scan→scatter building blocks and overflow reporting. No visible list, bin, page request,
  or work queue silently truncates; a coarser resident parent remains drawable under pressure.
- [ ] Add async-compute scheduling as a render-graph queue assignment for the same declared pass.
  Devices without useful overlap execute the identical pass on graphics.

## Global GPU data model

Indexed MDI cannot draw unrelated existing per-mesh buffers without CPU rebinding. Add:

- [ ] stable generational handles and global vertex/index/cluster/part/voxel/page arenas;
- [ ] BDA vertex pulling or global arena offsets for portable indexed draws;
- [ ] global immutable prototype, geometry, material, texture, coverage, skeleton, and page tables;
- [ ] per-draw records containing geometry, material, instance, deformation, representation, and
  cluster state, addressed by `drawID`/`firstInstance`/bin base rather than descriptor rebinding;
- [ ] a small fixed PSO-bin vocabulary for representation, coverage, sidedness, surface model,
  transparency, and pass;
- [ ] frame-safe upload rings, deferred handle/page reuse, stable in-flight addresses, and arena growth
  that retains old allocations until all referencing frames complete; and
- [ ] capability resolution that queries BDA, shader draw parameters, multi-draw, indirect count,
  descriptor-indexing subfeatures/limits, update-after-bind limits, `maxDrawIndirectCount`, subgroup
  properties, and mesh/task limits separately.

Missing optional limits affect scheduling/bin sizes, never content or quality.

## Thin-sheet foliage and coverage implementation

Implement the Phase-2 `.smat` contract in `MaterialAsset`, `MaterialParamsData`, codegen,
`SurfaceData`, material graph output, preview, thumbnails, all light paths, and RT hit shading:

- [ ] energy-conserving two-sided reflection plus thickness/absorption-based transmission with
  distinct front/back normal behavior;
- [ ] coverage-preserving alpha mip generation and one canonical coverage sample function;
- [ ] modeled geometry as the primary leaf/blade silhouette, alpha only for irreducible serrations/
  holes;
- [ ] A2C when MSAA is enabled and spatially anchored hashed coverage with deterministic temporal
  sequencing under TAA;
- [ ] coverage evaluation before expensive shading and identical depth/main/shadow/picking/RT
  classification; and
- [ ] aggregate material moments: occupancy/coverage density, albedo/roughness/transmission/thickness
  statistics, and a normal distribution sufficient for the same thin-sheet response.

Generic opaque/masked/translucent materials remain legitimate surface models. Thin-sheet foliage is
one `.smat` model, not a private plant material file or custom shader fork.

## Portable virtual hierarchy cooker

Replace the final sphere-only sequential meshlet cook contract with an independently specified,
portable hierarchy usable by ordinary `.smesh` and plant families:

- [ ] optimized triangle clusters with portable vertex/primitive limits, quantized local positions,
  compact local indices, octahedral normals/tangents, material class, bounds, normal cone,
  deformation influence, page ID, and child/parent error;
- [ ] hierarchy partitioning and simplification with watertight/solid rules for contiguous geometry;
- [ ] plant assembly tables retaining repeated semantic parts as micro-instance transforms rather
  than expanding each branch/leaf;
- [ ] aggregate voxel-brick clusters for disconnected foliage, storing occupancy and the coverage/
  material/normal moments above;
- [ ] an appearance error including silhouette, projected coverage, transmitted energy, material
  variation, and normal-distribution error—not geometry distance alone;
- [ ] hierarchy cuts that may mix triangle and voxel nodes and guarantee a drawable coarse root;
- [ ] page dependencies, parent-before-child order, conservative static/deformed bounds, and
  representation transition metadata; and
- [ ] portable voxel output as cooked indexed surfaces or compute expansion, so MoltenVK renders the
  same cut without mesh shaders.

Cluster semantics are device-independent. Runtime execution packing/workgroups use queried device
properties and do not require recooking.

## `.splantc` render artifact

Complete the derived plant TOC with part/prototype tables, triangle hierarchy, voxel hierarchy,
material/coverage tables, structural deformation metadata, page directory, guaranteed roots,
texture mip/KTX2-derived payloads where applicable, collision/RT derivation metadata, platform profile,
source hashes, checksums, and compression. `.splant` remains the only authored owner and recook source;
there is no independently renderable generated `.smesh` plant copy.

## Focused verification

- [ ] CPU hierarchy-cut reference proves child/parent coverage and no-hole residency behavior.
- [ ] Triangle↔voxel reference renders compare silhouette, coverage, transmission, material, and
  normal-distribution error over view/light directions.
- [ ] Coverage mip/A2C/hash classification agrees across all pass fixtures.
- [ ] Rust/Slang layouts, BDA offsets, handle generations, and indirect barriers are byte-locked.
- [ ] Arena growth/compaction under in-flight frames never invalidates an address or reuses a live
  generation.
- [ ] The portable executor can render every cooked representation in an isolated test on MoltenVK.
- [ ] Standard gate and virtual-geometry/material docs are green.

## NO-LEGACY gate

The old runtime remains the only production path until Phase 7; the new substrate is not hidden behind
an environment toggle. The virtual hierarchy schema is final before cutover, and no classic foliage
LOD/impostor format is introduced as scaffolding.

