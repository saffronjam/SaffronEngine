+++
title = 'Custom Slang shader'
weight = 2
+++

# Custom Slang shader

This tutorial writes a new Slang shader, lets the `xtask` shader pipeline compile it to SPIR-V,
and draws scene meshes with it through the renderer's PSO cache. The shader matches the engine's
mesh I/O contract, so it slots into the existing scene pass. Changing the fragment math changes
how every mesh using that material looks.

## How shaders get compiled

The `xtask shaders` task scans every `*.slang` under `engine/assets/shaders/` and compiles each
to `<name>.spv` next to the host binary, under `engine/target/<profile>/shaders/`:

```sh
cargo run -p xtask -- shaders
```

Per entry-point shader it runs:

```
slangc <shader>.slang -profile glsl_450 -target spirv -emit-spirv-directly \
        -fvk-use-entrypoint-name -matrix-layout-column-major -I <shader_dir> -o <shader>.spv
```

The shared `lighting.slang` is precompiled once to `lighting.slang-module` (`slangc … -emit-ir`,
no `.spv`); every other shader `import lighting` against it. Adding a `.slang` to the folder
compiles it with no code edit — the next pipeline run picks it up, and `just engine` runs the
pipeline right after the Cargo build, so a plain build sees it too. Both entry points live in
one `.slang` module, named by their `[shader(...)]` tag.

## Write the shader

A shader the scene pass can draw with honors the contract `mesh.slang` defines. Draws are
record-driven: the GPU visibility traversal emits one `GpuDrawRecord` per drawn slice and the
binner stores the record index in each command's `firstInstance`, so the vertex stage receives
that index as `SV_VulkanInstanceID` and pulls its geometry, instance transform, and material
through buffer device addresses in the GPU-scene address block. No pipeline declares a vertex
input. The fragment stage returns `SV_Target`.

Three set-2 bindings carry the contract: binding 2 is the material-parameter arena, binding 3
is the address block (`import global_gpu_data` declares it), and binding 4 is the record stream.

Create `engine/assets/shaders/flat.slang`:

```hlsl
// Draws each record in its material base color, lit by one hard-coded headlight.
// Pulls geometry and instance state through the GPU-scene address block, the same
// contract mesh.slang's executor entry points use, so the scene pass can draw with
// it unchanged.

import lighting;
import global_gpu_data;

// The active view's semantic record stream. `SV_VulkanInstanceID` indexes it.
[[vk::binding(4, 2)]] StructuredBuffer<GpuDrawRecord> executorRecords;

[shader("vertex")]
VertexOutput vertexMainExecutor(
    uint vertexIndex : SV_VulkanVertexID, uint recordIndex : SV_VulkanInstanceID)
{
    let addresses = gpuSceneAddresses;
    let record = executorRecords[recordIndex];
    let instance = gpuSceneLoadInstance(addresses, record.instance.index);
    let geometry = gpuSceneResidentGeometry(addresses, record.geometry);

    // The interleaved vertex is position, normal, uv0 — pulled from the global vertex
    // arena at the geometry's own range.
    uint64_t base = addresses.vertices + geometry.vertices.first
        + uint64_t(vertexIndex) * geometry.vertexStride;
    float3 local = *(float3*)(base);
    float3 localNormal = *(float3*)(base + 12u);
    float2 uv = *(float2*)(base + 24u);

    let materialReference = gpuSceneLoadMaterialReference(addresses, record.material.index);
    let material = gpuSceneResidentMaterial(addresses, materialReference.target);
    return transformExecutorVertex(
        local, localNormal, uv, instance, float3(0.0), material.parameterIndex,
        record.transition);
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    MaterialParams mat = materialParams[input.materialIndex];
    float3 n = normalize(input.worldNormal);
    float ndotl = saturate(dot(n, normalize(float3(0.3, 0.6, 1.0))));
    float3 shade = mat.baseColor.rgb * (0.2 + 0.8 * ndotl);
    return float4(shade, mat.baseColor.a);
}
```

`transformExecutorVertex` (from `lighting.slang`) composes the instance's world columns,
applies the camera push constant, and fills the `VertexOutput` interface every fragment entry
shades from — reuse it rather than transforming by hand, so your shader stays vertex-for-vertex
identical to the übershader.

> [!NOTE]
> The entry points must be named `vertexMainExecutor` and `fragmentMain` — that's what
> `build_mesh_pipeline` looks up when it builds the stage create-infos
> (`engine/crates/rendering/src/pipelines/`). Take the vertex index as
> `SV_VulkanVertexID`: Slang's D3D-flavoured `SV_VertexID` subtracts the command's
> `vertexOffset` back out, which loses a displaced row's slice base in the amplification
> arena.

## Build it

Run the shader pipeline so the new `.slang` compiles, then confirm the SPIR-V landed:

```sh
cargo run -p xtask -- shaders          # scans flat.slang, compiles it
ls engine/target/debug/shaders/flat.spv
```

If `slangc` rejects the file, the run fails with the line and message. The `.spv` under
`engine/target/debug/shaders/` is what the renderer loads at run time via
`asset_path("shaders/flat.spv")`.

## Draw with it

The renderer picks a pipeline per material. The renderer's `Material` carries a `shader` path
(default `"shaders/mesh.spv"`), and `request_executor_mesh_pipeline` builds and caches one PSO
per distinct `(shader, unlit)` key. Point a material at the new shader:

```rust
let mut flat = Material::default();
flat.shader = "shaders/flat.spv".to_string();   // the .spv you just compiled
// use `flat` as the material for the meshes you want drawn with it
```

The PSO cache builds `flat.spv` on first use and reuses it after. Check the pipeline count
once a mesh draws with it:

```sh
sa render-stats       # "pipelines" increments when flat.spv's PSO is built
```

> [!NOTE]
> The renderer's `Material::shader` is selected in engine code, not over the CLI — there's no
> `sa set-shader`. To see your shader live: set the shader path on the renderer `Material` in
> engine code, or edit the engine's `mesh.slang` in place so every mesh redraws with your
> changes on the next pipeline run. See
> [material and PSO selection](../../explanations/materials-and-pipelines/material-and-pso-selection/).

## Next

- [Übershader](../../explanations/materials-and-pipelines/ubershader-and-specialization/) — one shader for many materials via a spec constant.
- [Material and PSO selection](../../explanations/materials-and-pipelines/material-and-pso-selection/) — how the renderer's `Material::shader` becomes a cached pipeline.
- [Vertex layout](../../explanations/geometry-and-assets/mesh-and-vertex-layout/) — the arena layout your shader pulls through.
- [Descriptor sets](../../explanations/materials-and-pipelines/descriptor-sets/) — what sets 0–7 bind.
- [Render seams](../../explanations/app-lifecycle-and-window/the-submit-and-rendergraph-seams/) — adding a whole new pass from a layer.
