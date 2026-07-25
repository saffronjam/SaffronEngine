+++
title = 'Shadows & culling'
weight = 10
bookCollapseSection = true
+++

# Shadows & culling

Shadows and culling are the two visibility computations a forward renderer performs each frame. Every shadow-casting light renders depth into pages of one virtual shadow atlas; the mesh fragment resolves its position through a page table and compares against the stored depth. Clustered culling partitions the view frustum into cells and assigns each cell only the lights that touch it, narrowing the per-fragment light loop to nearby lights.

## Pages

| Page | Covers | Code |
|---|---|---|
| `virtual-shadow-maps` | the page atlas, residency, receiver demand, page raster | `vsm.rs` · `VsmResidency`; `renderer.rs` · `add_vsm_page_passes` |
| `directional-shadows` | camera-snapped clip levels, fine-to-coarse sampling | `vsm.rs` · `VsmDirectionalSpace`; `lighting_common.slang` · `vsmSampleDirectional` |
| `spot-light-shadows` | one shadowed spot's projective page space | `lighting.rs` · `set_spot_shadow`; `lighting_common.slang` · `vsmSampleSpot` |
| `point-light-cube-shadows` | six face spaces behind the cube major-axis mapping | `lighting.rs` · `point_shadow_face_matrices`; `lighting_common.slang` · `vsmSamplePoint` |
| `pcf-filtering` | comparison sampler, in-tile 3×3 kernel, the page gutter | `lighting_common.slang` · `vsmTilePcf` |
| `shadow-bias` | constant + slope bias, acne vs. peter-panning | `renderer.rs` · `add_vsm_page_passes`; `lighting.rs` · bias constants |
| `clustered-light-culling` | the froxel grid, exponential Z, sphere-vs-AABB cull dispatch | `light_cull.slang` · `computeMain` |
| `froxel-bounds` | screen-tile bounds → view-space AABB per froxel | `light_cull.slang` · `screenToView`/`rayToZ` |
