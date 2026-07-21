// Per-pixel screen-space motion vectors for TAA. Renders the instanced scene depth-tested;
// the fragment outputs (prevUV - curUV), i.e. the offset from this pixel's current UV to
// where the same surface point was last frame. RG16F. Set 2 = instances; push = current +
// previous camera viewProj.
//
// Two sources of motion are reprojected together:
//   - object motion: inst.model vs inst.prevModel (the whole entity moved).
//   - deformation motion (deforming meshes — skinned or morph): binding 0 = this frame's
//     deformed position, binding 1 = the previous frame's deformed position. For a static
//     mesh both bindings point at the same static stream, so prevPosition == position and
//     only object motion contributes.
// current clip = curViewProj  * model     * position
// previous clip = prevViewProj * prevModel * prevPosition

import coverage;
import material_params;

[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];

struct Instance
{
    float4x4 model;
    float4x4 normalMatrix;
    float4x4 prevModel;  // last frame's world matrix (object-motion reprojection)
    float4 baseColor;
    uint4 texture;
    float4 pbr;
    float4 emissive;
};
[[vk::binding(0, 2)]] StructuredBuffer<Instance> instances;
[[vk::binding(2, 2)]] StructuredBuffer<MaterialParams> materialParams;

struct Push
{
    float4x4 curViewProj;
    float4x4 prevViewProj;
};
[[vk::push_constant]] Push pc;

struct VertexInput
{
    [[vk::location(0)]] float3 position;      // binding 0: this frame's (deformed) position
    [[vk::location(1)]] float3 normal;
    [[vk::location(2)]] float2 uv0;
    [[vk::location(3)]] float3 prevPosition;  // binding 1: previous frame's (deformed) position
};

struct VertexOutput
{
    float4 position : SV_Position;
    float4 curClip : TEXCOORD0;
    float4 prevClip : TEXCOORD1;
    float3 worldPos : POSITION0;
    float2 uv0 : TEXCOORD2;
    nointerpolation uint materialIndex : TEXCOORD3;
    float3 coverageAnchor : TEXCOORD4;
};

[shader("vertex")]
VertexOutput vertexMain(VertexInput input, uint instanceIndex : SV_VulkanInstanceID)
{
    Instance inst = instances[instanceIndex];
    VertexOutput output;
    float4 curWorld = mul(inst.model, float4(input.position, 1.0));
    float4 prevWorld = mul(inst.prevModel, float4(input.prevPosition, 1.0));
    output.position = mul(pc.curViewProj, curWorld);
    output.curClip = output.position;
    output.prevClip = mul(pc.prevViewProj, prevWorld);
    output.worldPos = curWorld.xyz;
    output.uv0 = input.uv0;
    output.materialIndex = inst.texture.w;
    output.coverageAnchor = input.position;
    return output;
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    MaterialParams mat = materialParams[input.materialIndex];
    bool thinSheet = (mat.tex1.w & FEATURE_THIN_SHEET) != 0u;
    if (thinSheet || (mat.tex1.w & FEATURE_ALPHACLIP) != 0u)
    {
        float2 uv = input.uv0 * mat.uv.xy + mat.uv.zw;
        uint sourceIndex = thinSheet ? mat.coverage.x : mat.tex0.x;
        CoverageSample coverageSample = sampleCanonicalCoverage(
            albedoTextures[NonUniformResourceIndex(sourceIndex)],
            uv,
            input.coverageAnchor,
            thinSheet ? mat.coverage.y : COVERAGE_SOURCE_ALBEDO_ALPHA,
            thinSheet ? mat.coverage.z : COVERAGE_CLASS_MASKED,
            mat.baseColor.a,
            thinSheet ? mat.coverageHash.zw : uint2(1u),
            thinSheet ? mat.coverageHash.xy : uint2(0u),
            mat.tex1.z,
            mat.pbr.w,
            thinSheet,
            false);
        if (!coverageSample.covered) { discard; }
    }
    float2 curNdc = input.curClip.xy / input.curClip.w;
    float2 prevNdc = input.prevClip.xy / input.prevClip.w;
    // NDC -> UV is *0.5 (+0.5); the offset is the NDC delta scaled by 0.5. Both clips use
    // the same Y-flipped projection, so the Y sign is consistent with the sampled images.
    float2 motionUv = (prevNdc - curNdc) * 0.5;
    return float4(motionUv, 0.0, 0.0);
}
