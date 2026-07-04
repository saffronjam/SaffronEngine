// Thin G-buffer prepass for screen-space AO: renders view-space normal (rgb) + view-space
// Z (a, negative) into an rgba16f target and per-instance roughness into a second R8 target,
// and lays down depth. Instanced like the mesh / depth-prepass (set 2 = per-instance data);
// the push constant carries viewProj (clip) + view (to take positions/normals into view space).
// The roughness target feeds the specular-occlusion prepass (`specocc.slang`), whose reflection
// cone widens with roughness.

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

struct Push
{
    float4x4 viewProj;  // world -> clip
    float4x4 view;       // world -> view
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
    float3 viewNormal : NORMAL;
    float viewZ : TEXCOORD0;
    float roughness : TEXCOORD1;
};

[shader("vertex")]
VertexOutput vertexMain(VertexInput input, uint instanceIndex : SV_VulkanInstanceID)
{
    Instance inst = instances[instanceIndex];
    VertexOutput output;
    float4 worldPos = mul(inst.model, float4(input.position, 1.0));
    float4 viewPos = mul(pc.view, worldPos);
    output.position = mul(pc.viewProj, worldPos);
    float3 worldNormal = mul((float3x3)inst.normalMatrix, input.normal);
    output.viewNormal = mul((float3x3)pc.view, worldNormal);
    output.viewZ = viewPos.z;
    output.roughness = inst.pbr.y;
    return output;
}

// Two MRT targets: view normal + view-Z (rgba16f) and per-instance roughness (R8).
struct GbufferOut
{
    float4 normalZ : SV_Target0;
    float roughness : SV_Target1;
};

[shader("fragment")]
GbufferOut fragmentMain(VertexOutput input, bool isFront : SV_IsFrontFace)
{
    GbufferOut o;
    // Double-sided geometry rasterizes both faces; invert the back-face normal (glTF) so the
    // screen-space AO/GI/SSR that read this G-buffer see the actually-visible face. Single-sided
    // geometry culls back faces, so `isFront` is always true and this is a no-op.
    float3 viewNormal = normalize(input.viewNormal);
    if (!isFront) { viewNormal = -viewNormal; }
    o.normalZ = float4(viewNormal, input.viewZ);
    o.roughness = input.roughness;
    return o;
}
