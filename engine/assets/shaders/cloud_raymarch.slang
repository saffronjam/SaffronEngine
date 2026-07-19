// Adaptive reduced-resolution cloud march. One 4x4 phase is freshly evaluated each frame; empty
// space advances through a conservative low-detail gate, while occupied samples evaluate the full
// eroded density and energy-conserving lighting integral.

import clouds;
import cloud_lighting;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> cloudReduced;
[[vk::image_format("r16f")]]
[[vk::binding(1, 0)]] RWTexture2D<float> cloudDepth;
[[vk::binding(2, 0)]] Sampler2D sceneDepth;
[[vk::binding(3, 0)]] Sampler3D<float4> baseNoise;
[[vk::binding(4, 0)]] Sampler3D<float4> detailNoise;
[[vk::binding(5, 0)]] Sampler2D<float4> curlNoise;
[[vk::binding(6, 0)]] Sampler2D<float4> weatherMap;
[[vk::binding(7, 0)]] Sampler2D<float4> transmittanceLut;
[[vk::binding(8, 0)]] StructuredBuffer<float4> skySh;
[[vk::binding(9, 0)]] ConstantBuffer<CloudParams> params;
[[vk::binding(10, 0)]] Sampler2DArray<float> cloudShadow;

static const float CLOUD_MARCH_FAR = 200000.0;
static const float CLOUD_MARCH_EXTINCTION = 0.002;

float cloudRayJitter(uint2 pixel, uint frameIndex)
{
    static const uint bayer[16] =
    {
        0u, 8u, 2u, 10u,
        12u, 4u, 14u, 6u,
        3u, 11u, 1u, 9u,
        15u, 7u, 13u, 5u
    };
    uint phase = (pixel.x & 3u) | ((pixel.y & 3u) << 2u);
    float spatial = (float(bayer[phase]) + 0.5) / 16.0;
    float temporal = frac(float(frameIndex + 1u) * 0.61803398875);
    return frac(spatial + temporal);
}

bool cloudLayerSegment(float3 origin, float3 direction, out float tNear, out float tFar)
{
    if (abs(direction.y) < 1e-6)
    {
        tNear = 0.0;
        tFar = 0.0;
        return false;
    }
    float t0 = (params.layer.x - origin.y) / direction.y;
    float t1 = (params.layer.x + params.layer.y - origin.y) / direction.y;
    tNear = max(min(t0, t1), 0.0);
    tFar = min(max(t0, t1), CLOUD_MARCH_FAR);
    return tFar > tNear;
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    cloudReduced.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    uint phase = (tid.x & 3u) | ((tid.y & 3u) << 2u);
    if (phase != (params.frame.x & 15u))
    {
        return;
    }

    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);
    float4 farH = mul(params.invViewProj, float4(uv * 2.0 - 1.0, 1.0, 1.0));
    float3 origin = params.cameraCoverage.xyz;
    float3 direction = normalize(farH.xyz / farH.w - origin);
    float tNear;
    float tFar;
    if (!cloudLayerSegment(origin, direction, tNear, tFar))
    {
        cloudReduced[tid.xy] = float4(0.0, 0.0, 0.0, 1.0);
        cloudDepth[tid.xy] = 65504.0;
        return;
    }

    float deviceDepth = sceneDepth.SampleLevel(uv, 0.0).r;
    if (deviceDepth < 0.999999)
    {
        float4 receiverH = mul(params.invViewProj, float4(uv * 2.0 - 1.0, deviceDepth, 1.0));
        float3 receiver = receiverH.xyz / receiverH.w;
        tFar = min(tFar, length(receiver - origin));
    }
    if (tFar <= tNear)
    {
        cloudReduced[tid.xy] = float4(0.0, 0.0, 0.0, 1.0);
        cloudDepth[tid.xy] = min(tFar, 65504.0);
        return;
    }

    uint primarySteps = clamp(uint(params.march.x + 0.5), 1u, 256u);
    float fineStep = max((tFar - tNear) / float(primarySteps), 8.0);
    float coarseStep = fineStep * 4.0;
    float t = tNear + cloudRayJitter(tid.xy, params.frame.x) * fineStep;
    float3 scatter = float3(0.0);
    float transmittance = 1.0;
    float depthNumerator = 0.0;
    float depthDenominator = 0.0;
    uint emptySamples = 0u;
    uint samples = 0u;
    uint pixelHash = cloudHash(uint3(tid.xy, params.frame.x));

    while (t < tFar && transmittance > 0.01 && samples < primarySteps)
    {
        float3 worldPos = origin + direction * t;
        float conservative = sampleCloudConservativeDensity(
            worldPos, params, baseNoise, weatherMap);
        if (conservative <= 1e-4)
        {
            t += coarseStep * lerp(0.5, 1.0, saturate(1.0 - conservative));
            emptySamples = 0u;
            continue;
        }

        float density = sampleCloudDensity(
            worldPos, params, baseNoise, detailNoise, curlNoise, weatherMap);
        samples += 1u;
        if (density <= 1e-4)
        {
            emptySamples += 1u;
            t += emptySamples > 4u ? coarseStep : fineStep;
            continue;
        }
        emptySamples = 0u;
        float sigmaT = density * CLOUD_MARCH_EXTINCTION;
        float heightFraction = saturate((worldPos.y - params.layer.x) / params.layer.y);
        float3 incident = cloudIncidentLighting(
            worldPos,
            direction,
            heightFraction,
            pixelHash + samples,
            params,
            baseNoise,
            detailNoise,
            curlNoise,
            weatherMap,
            transmittanceLut,
            skySh,
            cloudShadow);
        float sliceT = exp(-sigmaT * fineStep);
        float3 source = sigmaT * incident;
        float3 integrated = source * (1.0 - sliceT) / max(sigmaT, 1e-7);
        float opacityWeight = transmittance * (1.0 - sliceT);
        scatter += transmittance * integrated;
        depthNumerator += opacityWeight * t;
        depthDenominator += opacityWeight;
        transmittance *= sliceT;
        t += fineStep;
    }

    float meanDepth = depthDenominator > 1e-5
        ? depthNumerator / depthDenominator
        : min(tFar, 65504.0);
    cloudReduced[tid.xy] = float4(max(scatter, float3(0.0)), saturate(transmittance));
    cloudDepth[tid.xy] = min(meanDepth, 65504.0);
}
