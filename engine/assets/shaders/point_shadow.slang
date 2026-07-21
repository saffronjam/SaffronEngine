// Point-light shadow: renders the scene's WORLD-SPACE DISTANCE from the light into one
// face of a color cubemap (6 passes, one per face). The mesh fragment then samples the
// cube by the light->fragment direction and compares linear distance. Instanced like the
// mesh/depth-prepass (set 2 = per-instance data); the push constant carries this face's
// viewProj + the light world position.

import coverage;
import material_params;

[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];

struct Instance
{
    float4x4 model;
    float4x4 normalMatrix;
    float4x4 prevModel;
    float4 baseColor;
    uint4 texture;
    float4 pbr;
    float4 emissive;
};
[[vk::binding(0, 2)]] StructuredBuffer<Instance> instances;
[[vk::binding(2, 2)]] StructuredBuffer<MaterialParams> materialParams;

struct Push
{
    float4x4 viewProj;  // this cube face's world -> clip
    float4 lightPos;    // xyz = light world position
};
[[vk::push_constant]] Push pc;

struct VertexInput
{
    [[vk::location(0)]] float3 position;
    [[vk::location(1)]] float3 normal;
    [[vk::location(2)]] float2 uv0;
};

struct VertexOutput
{
    float4 position : SV_Position;
    float3 worldPos : POSITION0;
    float2 uv0 : TEXCOORD0;
    nointerpolation uint materialIndex : TEXCOORD1;
    float3 coverageAnchor : TEXCOORD2;
};

[shader("vertex")]
VertexOutput vertexMain(VertexInput input, uint instanceIndex : SV_VulkanInstanceID)
{
    Instance inst = instances[instanceIndex];
    VertexOutput output;
    float4 worldPos = mul(inst.model, float4(input.position, 1.0));
    output.worldPos = worldPos.xyz;
    output.position = mul(pc.viewProj, worldPos);
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
    float dist = length(input.worldPos - pc.lightPos.xyz);
    return float4(dist, 0.0, 0.0, 1.0);
}
