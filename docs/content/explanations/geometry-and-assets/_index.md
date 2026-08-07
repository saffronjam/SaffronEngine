+++
title = 'Geometry & assets'
weight = 5
bookCollapseSection = true
+++

# Geometry & assets

The asset path turns a model on disk into triangles the GPU can draw. Import reads glTF and OBJ
into one common `Mesh` and bakes it into a versioned `.smodel` container (a `.smesh` mesh image
plus its materials, textures, and clips); the asset server then keys assets by UUID, caches
their GPU resources, names them in a catalog, and feeds the persistent GPU scene. The `saffron-geometry`
crate owns the CPU types and byte codecs; `saffron-assets` owns the catalog, import/bake, and
`render_scene`.

## Pages

| Page | Covers | Code |
|---|---|---|
| `mesh-and-vertex-layout` | `Vertex` (pos/normal/uv/tangent), `Mesh`, `Submesh`, the pinned 48-byte stride | `geometry/src/types.rs` · `Vertex`, `Mesh` |
| `built-in-primitives` | native cube/plane/sphere as reserved-id meshes, never catalog assets | `geometry/src/primitives.rs`; `assets/src/lib.rs` · `BuiltinMesh` |
| `gltf-and-obj-import` | the `gltf` + `tobj` crates into one common `ImportedModel` | `geometry/src/*_import.rs` · `translate_model` |
| `smesh-format` | the baked, versioned binary mesh image | `geometry/src/smesh.rs` · `save_mesh_to_buffer`, `load_mesh_from_bytes` |
| `sanim-format` | the baked, versioned animation-clip image | `geometry/src/sanim.rs` · `save_animation`, `load_animation` |
| `image-decoding` | the `image` crate → RGBA8 / linear-float, embedded textures | `geometry/src/image_decode.rs` · `decode_image` |
| `gpu-mesh-upload` | VMA staging, `GpuMesh`, mesh AABB bounds | `rendering/src/upload.rs` · `upload_mesh` |
| `asset-server-and-catalog` | `AssetServer`, UUID→GPU negative-caches, the named/renameable catalog | `assets/src/lib.rs` · `AssetServer` |
| `asset-mutation-journal` | revisioned catalog/cache changes and derived-state invalidation | `assets/src/journal.rs` · `AssetMutation` |
| `import-pipeline` | `import_model` / `import_texture`, baking, dedup | `assets/src/import.rs` |
| `draw-list` | GPU record stream → draw buckets → counted indirect executor draws | `rendering/src/visibility.rs`; `rendering/src/scene_pass.rs` · `record_executor_buckets` |
| `project-serialization` | project folders, `project.json`, app-data startup, local assets | `assets/src/project.rs`; `control/src/commands_asset.rs` |
| `smodel-container` | one self-contained model container; header/TOC/metadata, scan, extract, reimport | `geometry/src/smodel.rs`; `assets/src/import.rs` · `write_container`, `bake_model`, `scan_assets` |
| `vegetation-assets` | Plant families, biome graphs, and sparse vegetation maps | `vegetation/src/asset.rs` · `PlantFamilyAsset`, `BiomeAsset`, `VegetationMapAsset` |
| `botanical-graph` | Typed authoring IR that grows a plant family, plus its manual edit layer | `vegetation/src/botanical.rs` · `grow`, `BotanicalGraphDocument` |
| `point-interchange` | Instanced points in and out of content-creation tools | `vegetation/src/interchange.rs` · `interchange_to_anchors`, `read_houdini_points` |
| `biome-graph-evaluation` | Typed biome compilation, deterministic cells, executors, and provenance | `vegetation/src/graph.rs` · `BiomeGraphEvaluator` |
| `vegetation-cooking` | Staged plant, cell, and manifest cooking, and the two roots it writes to | `vegetation/src/cook.rs` · `CookGraph`, `VegetationBaseManifest` |
| `virtual-geometry` | Portable triangle/voxel hierarchy and global GPU records | `geometry/src/virtual_hierarchy.rs`; `rendering/src/global_gpu_data.rs` |
| `plant-rendering` | Cooked families to GPU instances: assembly, macro adapter, micro fields | `assets/src/plant_render.rs` · `load_plant_family`, `sync_vegetation` |
