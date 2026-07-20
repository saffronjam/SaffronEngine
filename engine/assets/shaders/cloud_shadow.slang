// Cascaded density-integrated Beer/ESM cloud shadow fill. One thread per texel/layer.

import clouds;

[[vk::image_format("r16f")]]
[[vk::binding(0, 0)]] RWTexture2DArray<float> cloudShadow;
[[vk::binding(1, 0)]] Sampler3D<float4> baseNoise;
[[vk::binding(2, 0)]] Sampler3D<float4> detailNoise;
[[vk::binding(3, 0)]] Sampler2D<float4> curlNoise;
[[vk::binding(4, 0)]] Sampler2D<float4> weatherMap;
[[vk::binding(5, 0)]] ConstantBuffer<CloudParams> params;

static const float CLOUD_SHADOW_EXTINCTION = 0.002;
static const uint CLOUD_SHADOW_STEPS = 64u;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    uint layers;
    cloudShadow.GetDimensions(width, height, layers);
    if (tid.x >= width || tid.y >= height || tid.z >= layers)
    {
        return;
    }

    float2 planeUv = ((float2(tid.xy) + 0.5) / float2(width, height)) * 2.0 - 1.0;
    float4 centerExtent = params.shadowCenters[tid.z];
    float3 linePoint = centerExtent.xyz
        + params.shadowRight.xyz * planeUv.x * centerExtent.w
        + params.shadowUp.xyz * planeUv.y * centerExtent.w;
    float3 toLight = normalize(params.shadowLight.xyz);
    float invY = 1.0 / max(abs(toLight.y), 1e-4) * sign(toLight.y);
    float t0 = (params.layer.x - linePoint.y) * invY;
    float t1 = (params.layer.x + params.layer.y - linePoint.y) * invY;
    float tNear = min(t0, t1);
    float tFar = max(t0, t1);
    float stepLength = (tFar - tNear) / float(CLOUD_SHADOW_STEPS);
    float opticalDepth = 0.0;
    for (uint step = 0u; step < CLOUD_SHADOW_STEPS; ++step)
    {
        float t = tNear + (float(step) + 0.5) * stepLength;
        float density = sampleCloudDensity(
            linePoint + toLight * t,
            params,
            baseNoise,
            detailNoise,
            curlNoise,
            weatherMap);
        opticalDepth += density * stepLength;
    }
    float beer = exp(-opticalDepth * CLOUD_SHADOW_EXTINCTION);
    cloudShadow[tid] = exp(params.shadowLight.w * beer);
}
