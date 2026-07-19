// Unlit cloud-density debugger. It integrates raw density through the authored layer with a fixed
// 48-step Beer-Lambert march, stopping at opaque depth, and overwrites scene color with grayscale.

import clouds;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> target;
[[vk::binding(1, 0)]] Sampler2D depthTex;
[[vk::binding(2, 0)]] Sampler3D<float4> baseNoise;
[[vk::binding(3, 0)]] Sampler3D<float4> detailNoise;
[[vk::binding(4, 0)]] Sampler2D<float4> curlNoise;
[[vk::binding(5, 0)]] Sampler2D<float4> weatherMap;
[[vk::binding(6, 0)]] ConstantBuffer<CloudParams> params;

static const uint DEBUG_STEPS = 48u;
static const float DEBUG_FAR = 200000.0;
static const float DEBUG_EXTINCTION = 0.002;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    target.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);
    float depth = depthTex.SampleLevel(uv, 0.0).r;
    float4 farH = mul(params.invViewProj, float4(uv * 2.0 - 1.0, 1.0, 1.0));
    float3 rayDir = normalize(farH.xyz / farH.w - params.cameraCoverage.xyz);

    float y0 = params.layer.x;
    float y1 = y0 + params.layer.y;
    if (abs(rayDir.y) < 1e-6)
    {
        target[tid.xy] = float4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    float t0 = (y0 - params.cameraCoverage.y) / rayDir.y;
    float t1 = (y1 - params.cameraCoverage.y) / rayDir.y;
    float tNear = max(min(t0, t1), 0.0);
    float tFar = min(max(t0, t1), DEBUG_FAR);

    if (depth < 0.999999)
    {
        float4 receiverH = mul(params.invViewProj, float4(uv * 2.0 - 1.0, depth, 1.0));
        float3 receiver = receiverH.xyz / receiverH.w;
        tFar = min(tFar, length(receiver - params.cameraCoverage.xyz));
    }
    if (tFar <= tNear)
    {
        target[tid.xy] = float4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    float dt = (tFar - tNear) / float(DEBUG_STEPS);
    float transmittance = 1.0;
    for (uint i = 0u; i < DEBUG_STEPS; ++i)
    {
        float t = tNear + (float(i) + 0.5) * dt;
        float3 worldPos = params.cameraCoverage.xyz + rayDir * t;
        float density = sampleCloudDensity(
            worldPos, params, baseNoise, detailNoise, curlNoise, weatherMap);
        transmittance *= exp(-density * dt * DEBUG_EXTINCTION);
    }
    float coverage = 1.0 - transmittance;
    target[tid.xy] = float4(coverage, coverage, coverage, 1.0);
}
