// Minimal mesh-thumbnail shader: a single mesh lit by a fixed directional light, flat
// neutral albedo. No instancing/textures/light buffers — the push constant carries the
// mvp + normal matrix. Used by renderMeshThumbnail to preview a mesh in the asset panel.

import tonemap_ops;

struct VertexInput
{
    [[vk::location(0)]] float3 position;
    [[vk::location(1)]] float3 normal;
    [[vk::location(2)]] float2 uv0;
};

struct VertexOutput
{
    float4 position : SV_Position;
    float3 worldNormal : NORMAL;
};

struct PushConstants
{
    float4x4 mvp;
    float4x4 normalMatrix;
};
[[vk::push_constant]] PushConstants pc;

[shader("vertex")]
VertexOutput vertexMain(VertexInput input)
{
    VertexOutput output;
    output.position = mul(pc.mvp, float4(input.position, 1.0));
    output.worldNormal = mul((float3x3)pc.normalMatrix, input.normal);
    return output;
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    float3 n = normalize(input.worldNormal);
    float3 lightDir = normalize(float3(-0.4, -1.0, -0.5));
    float ndotl = max(dot(n, -lightDir), 0.0);
    float3 albedo = float3(0.80, 0.80, 0.82);
    float3 color = albedo * (0.28 + 0.72 * ndotl);
    // sRGB encode (+ PBR Neutral), shared with the other thumbnail tiles + the viewport tonemap
    // (the UNORM target has no hardware OETF, so it is applied here).
    color = tonemapAndEncode(color, 3u);
    return float4(color, 1.0);
}
