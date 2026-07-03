+++
title = 'Materials & pipelines'
weight = 7
bookCollapseSection = true
+++

# Materials & pipelines

A material is the surface description a mesh draws with — its shader and its parameters — and a pipeline is the compiled GPU state that renders it. Materials are first-class, editable assets (`.smat`), assignable to entities and authorable in a node graph. These pages explain how a material resolves to a pipeline and how the pipeline count stays small through four mechanisms:

- One übershader covers every material.
- A specialization constant adds the unlit variant.
- A single bindless texture array lets draws that differ only by texture batch together.
- A node graph folds to params where it can, and only codegens a per-graph shader when it must.

## Pages

| Page | Covers | Code |
|---|---|---|
| `material-and-pso-selection` | the `Material` (shader + variant), alpha-mode axes (blend / alpha-to-coverage), `request_mesh_pipeline`, build-on-miss cache | `pipelines.rs` · `request_mesh_pipeline`, `PsoKey` |
| `ubershader-and-specialization` | one `mesh.slang`, `[[vk::constant_id]]` unlit + alpha-to-coverage permutations, skinned/wireframe variants | `pipelines.rs` · `build_mesh_pipeline`; `mesh.slang` · `kUnlit`, `kAlphaToCoverage` |
| `descriptor-sets` | set 0 bindless, set 1 lighting, set 2 instances, set 3 IBL, set 4 screen-space | `lighting.slang` · `vk::binding`; `descriptors.rs` |
| `bindless-textures` | one albedo array (PARTIALLY_BOUND + UPDATE_AFTER_BIND), `upload_texture` slot, per-instance index | `descriptors.rs` · `claim_slot`, `write_texture`; `upload.rs` · `upload_texture` |
| `native-materials` | `.smat` assets, the params buffer + `evalSurface` seam, PBR slots, instances, the editor | `material.rs` · `MaterialAsset`; `mesh.slang` · `evalSurface` |
| `node-graph-codegen` | graph fold-vs-codegen, the Slang emitter, `slangc` → per-graph PSO, the React Flow editor | `graph.rs` · `emit_graph_surface`; `MaterialGraphEditor.tsx` |
