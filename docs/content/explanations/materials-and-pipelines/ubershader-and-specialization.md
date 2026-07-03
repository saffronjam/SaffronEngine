+++
title = 'Übershader'
weight = 2
+++

# Übershader

An übershader is a single shader source that covers many material features, with each feature toggled by a compile-time constant rather than a separate file. The engine builds every variant from one mesh shader, `mesh.slang`, that almost every renderable passes through.

The lit and unlit paths compile as two specializations of the same SPIR-V, selected by a Vulkan specialization constant. The pipeline cache stays small: N materials share one PSO, and each extra variant adds one more.

## A constant-gated branch

The übershader declares a boolean specialization constant and branches on it at the top of the fragment stage:

```hlsl
[[vk::constant_id(0)]] const bool kUnlit = false;

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    MaterialInput mi = makeMaterialInput(input);
    SurfaceData surf = evalSurface(mi);

    if (kUnlit)
    {
        return float4(surf.albedo + surf.emissive, surf.opacity);
    }
    return evalLighting(input, surf);   // full Cook-Torrance lighting, IBL, shadows, screen-space terms
}
```

`kUnlit` is not a uniform. A specialization constant resolves when the pipeline is created, so the branch folds away at PSO compile time. The lit PSO holds no unlit code, and the unlit PSO holds none of the [BRDF](../../lighting-and-brdf/cook-torrance-brdf/), IBL, or shadow work. There is no per-fragment branch cost and no dynamic-uniform read.

The Rust side supplies the value through a `vk::SpecializationInfo` when `build_mesh_pipeline` builds the fragment stage — a `vk::SpecializationMapEntry` per constant, matching the `[[vk::constant_id(N)]]` declarations. The same shader module produces a lit pipeline when `unlit` is false and an unlit one when true, and each value becomes its own cache entry (`PsoKey::unlit`).

## The alpha-to-coverage permutation

A second fragment spec constant, `[[vk::constant_id(1)]] const bool kAlphaToCoverage`, handles anti-aliased masked cutouts. A masked material (glTF `MASK`) normally does a hard 1-bit `discard` against the cutoff, which aliases badly on foliage edges. Under MSAA, the masked-under-MSAA PSO instead sets `alpha_to_coverage_enable` on the multisample state and bakes `kAlphaToCoverage = true`; the fragment then rescales the cutout alpha around the cutoff by its screen-space derivative (`saturate((opacity - cutoff)/max(fwidth(opacity), 1e-4) + 0.5)`) and returns it as `SV_Target.a`, which the hardware turns into a per-sample coverage mask. It is order-independent and needs no blending or sorting — the modern foliage path. The constant folds away like `kUnlit`, so the opaque and blend PSOs carry none of the coverage math.

## The skinned and wireframe permutations

Two more permutations ride the same key without new files. **Skinned** binds the `vertexMainSkinned` entry point and adds the `VertexSkin` stream (joints + weights) on binding 1; the base layout (binding 0) is untouched, so the unskinned PSO is unchanged. **Wireframe** sets `vk::PolygonMode::LINE`, gated on the `fill_mode_non_solid` capability. Each is one more boolean in `PsoKey` and one more cache entry.

## Why specialization, not a branch or two files

A uniform branch keeps one PSO but pays a runtime branch and forces both code paths to stay live in the binary, raising register pressure and lowering occupancy. Two separate shader files remove the runtime cost but duplicate the shared code — vertex layout, vertex stage, the bindless sample — and become two files to edit in lockstep.

A specialization constant keeps a single source of truth and gives each variant a fully specialized binary with the dead path compiled out. The approach generalizes, and alpha-to-coverage is exactly that generalization realized: one more `vk::constant_id`, one more shader branch, and one derived `PsoKey` field — no new file and no new pipeline-building code.

## In the code

| What | File | Symbols |
|---|---|---|
| Constant + gated branch | `mesh.slang` | `kUnlit`, `kAlphaToCoverage`, `fragmentMain`, `vertexMainSkinned` |
| Baking the constant | `pipelines.rs` | `build_mesh_pipeline` — `SpecializationInfo`, `SpecializationMapEntry` |
| Variant → cache key | `pipelines.rs` | `PsoKey`, `request_mesh_pipeline` |

## Related

- [Materials & PSOs](../material-and-pso-selection/) — how a variant maps to a cache entry
- [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/) — the lit path the constant compiles out
- [Bindless textures](../bindless-textures/) — the albedo sample shared by both variants
