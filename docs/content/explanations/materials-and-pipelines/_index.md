+++
title = 'Materials & pipelines'
weight = 7
bookCollapseSection = true
+++

# Materials & pipelines

A material contains the surface parameters and shader identity used for a draw. A pipeline state object (PSO) contains the compiled GPU state that renders it. Anima stores authored materials as `.smat` assets, assigns them to entity submeshes through `MaterialSet`, and resolves them into compact GPU records and cached PSOs.

The shared `mesh.slang` shader covers fixed PBR materials. Specialization constants and PSO state select unlit, alpha-to-coverage, blend, skinning, and wireframe permutations. Texture slots index one bindless descriptor array, so texture identity stays out of the PSO key. A node graph either folds into ordinary parameters or supplies a generated shader identity.

## Pages

| Page | Covers | Code |
|---|---|---|
| [Materials & PSOs](material-and-pso-selection/) | Material flags, typed PSO keys, lazy construction, and cache reuse | `pipelines.rs` · `request_mesh_pipeline`, `PsoKey` |
| [Übershader](ubershader-and-specialization/) | Shared mesh shader and its specialized pipeline permutations | `pipelines.rs` · `build_mesh_pipeline`; `mesh.slang` · `kUnlit`, `kAlphaToCoverage` |
| [Descriptor sets](descriptor-sets/) | Mesh resource layout across bindless, lighting, material, and feature sets | `lighting.slang` · `vk::binding`; `pipelines.rs` · `Pipelines::new` |
| [Bindless textures](bindless-textures/) | Descriptor indexing, slot allocation, reclamation, and per-material texture indices | `descriptors.rs` · `claim_slot`, `write_texture`; `upload.rs` · `upload_texture` |
| [Native materials](native-materials/) | Material assets, inheritance, entity slots, and GPU parameter records | `material.rs` · `MaterialAsset`; `render_material.rs` · `resolve_entity_materials` |
| [Node-graph codegen](node-graph-codegen/) | Parameter folding, Slang emission, generated variants, and editor graph flow | `graph.rs` · `lower_graph_to_params`, `emit_graph_surface`; `MaterialGraphEditor.tsx` |
