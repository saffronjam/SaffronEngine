+++
title = 'Übershader'
weight = 2
+++

# Übershader

An übershader puts the common mesh surface and lighting paths in one shader module. Anima uses `mesh.slang` for the standard metallic-roughness material, then controls its broad shading modes with Vulkan specialization constants and its texture-dependent features with a per-material bitfield.

This arrangement avoids a shader module for every combination of normal mapping, emissive maps, alpha testing, and height treatment. Those features can vary through material-table data while compatible draws share a [pipeline](../material-and-pso-selection/).

## Three specialized modes

[`mesh.slang`](https://shader-slang.org/) declares three fragment-stage constants with stable Vulkan IDs:

```hlsl
[[vk::constant_id(0)]] const bool kUnlit = false;
[[vk::constant_id(1)]] const bool kAlphaToCoverage = false;
[[vk::constant_id(2)]] const bool kTranslucent = false;
```

A [specialization constant](https://registry.khronos.org/vulkan/specs/latest/html/vkspec.html#pipelines-specialization-constants) receives its value when the PSO is created. The driver can optimize the specialized shader for that fixed value, without requiring a separate SPIR-V file for each combination.

`build_mesh_pipeline_with_module` supplies the constants as three `VkBool32` values in a 12-byte `vk::SpecializationInfo` payload. The map entries bind offsets 0, 4, and 8 to constant IDs 0, 1, and 2. All three values also participate in PSO selection through `PsoKey` fields or derived fields.

| Constant | Selected by | Shader effect |
|---|---|---|
| `kUnlit` | `Material::unlit` | Returns albedo plus emissive instead of evaluated lighting |
| `kAlphaToCoverage` | Masked material with multisampling | Converts the alpha-test edge to sample coverage |
| `kTranslucent` | `Material::blend` | Avoids opaque screen-space terms and uses world-space indirect light |

The translucent mode matters because the opaque buffers at a transparent fragment's screen coordinate describe the geometry behind it. The shader excludes those screen-space lighting terms and evaluates indirect light that belongs to the translucent surface itself.

## Alpha-to-coverage

[Alpha-to-coverage](https://registry.khronos.org/vulkan/specs/latest/man/html/VkPipelineMultisampleStateCreateInfo.html) turns fragment alpha into an implementation-dependent multisample coverage mask. For a masked material under MSAA, the canonical classifier returns a filtered coverage probability:

```hlsl
CoverageSample coverage = sampleCanonicalCoverage(
    source, uv, coverageAnchor, sourceKind, classification, baseColorAlpha,
    sourceExtent, salt, temporalPhase, cutoff, canonicalProbability, true
);
if (!coverage.covered) { discard; }
lit.a = coverage.alpha;
```

The matching PSO enables `alpha_to_coverage_enable`. Masked surfaces at one sample use the hard alpha test, while opaque and translucent surfaces leave this specialization false. Alpha-to-coverage does not enable color blending or require back-to-front sorting.

## Data-driven features

Texture-dependent material features use bits in `MaterialParamsData` rather than specialization constants. `evalSurface` tests the bits before sampling optional resources or applying a height mode.

| Bit | Surface path |
|---|---|
| `FEATURE_NORMAL` | Tangent-space normal map |
| `FEATURE_EMISSIVE_TEX` | Emissive texture |
| `FEATURE_OCCLUSION` | Occlusion map |
| `FEATURE_HEIGHT` | Parallax occlusion mapping |
| `FEATURE_ALPHACLIP` | Masked alpha test |
| `FEATURE_DISPLACE` | Fine normal for compute-displaced geometry |
| `FEATURE_HEIGHT_BUMP` | Height-derived shading normal |
| `FEATURE_THIN_SHEET` | Two-sided foliage reflection, transmission, and coverage data |

The feature word travels with the resolved material-table row. Two surfaces can therefore differ in these paths and still use the same shader module and PSO. Texture indices select their resources from the [bindless arrays](../bindless-textures/).

## Other PSO axes

The geometry stage is another PSO axis — the `mesh_shader` field of `PsoKey`, not a fragment specialization constant. Every raster PSO binds no vertex input: each draw record resolves its geometry through buffer device addresses, whether that is the page arena, the per-frame deformed buffers holding compute-skinned and morphed vertices, or the displacement arena. `vertexMainExecutor` is the vertex-stage entry and `meshMainExecutor` the mesh-stage one, over the identical records.

Wireframe is another PSO axis rather than shader specialization. It chooses `VK_POLYGON_MODE_LINE` when the device supports non-solid fill. Sample count and fixed-function blend state also belong to the pipeline key.

## Generated material shaders

A node graph that folds to standard material inputs stays on `mesh.spv`. A graph with custom surface operations splices its emitted `evalSurface` body between the graph markers in `mesh.slang`, then compiles `materials/<uuid>_mesh.spv`.

The generated module retains the same entry points, specialization IDs, descriptor interface, and lighting code. Its absolute shader path becomes a distinct PSO-key value, so only draws using that generated surface module share its pipelines.

Shader compilation also emits a `_nort.spv` sibling with `SAFFRON_NO_RT=1`. A device without ray tracing loads that module so the declared shader interface matches the pipeline layout without ray-tracing descriptor sets.

## In the code

| What | File | Symbols |
|---|---|---|
| Surface and specialized modes | `mesh.slang` | `evalSurface`, `kUnlit`, `kAlphaToCoverage`, `kTranslucent` |
| Material feature bits | `material_params.slang` | `FEATURE_NORMAL`, `FEATURE_ALPHACLIP`, `FEATURE_THIN_SHEET` |
| Canonical coverage | `coverage.slang` | `sampleCanonicalCoverage`, `classifyCanonicalCoverage` |
| Specialization payload | `pipelines.rs` | `PsoKey`, `build_mesh_pipeline_with_module` |
| Generated mesh module | `codegen.rs` | `compile_material_mesh_shader`, `splice_mesh_source` |
| Ray-tracing-off compilation | `shaders.rs` | `NO_RT_DEFINE`, `NO_RT_SUFFIX` |

## Related

- [Materials & PSOs](../material-and-pso-selection/) — cache keys and pipeline reuse
- [Node graph code generation](../node-graph-codegen/) — graph folding and generated surface bodies
- [Bindless textures](../bindless-textures/) — texture selection within the shared shader
- [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/) — the standard lit path
