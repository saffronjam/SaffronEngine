// Depth-aware bilateral cloud upscale into the full-resolution cloud ledger inputs.

import clouds;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> cloudFullColor;
[[vk::binding(1, 0)]] Sampler2D<float4> cloudReduced;
[[vk::binding(2, 0)]] Sampler2D<float> cloudReducedDepth;
[[vk::binding(3, 0)]] Sampler2D<float> sceneDepth;
[[vk::image_format("r32f")]]
[[vk::binding(4, 0)]] RWTexture2D<float> cloudFullDepth;
[[vk::binding(5, 0)]] ConstantBuffer<CloudParams> params;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    cloudFullColor.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);
    float deviceDepth = sceneDepth.SampleLevel(uv, 0.0);
    float sceneDistance = 1e9;
    if (deviceDepth < 0.999999)
    {
        float4 receiverH = mul(params.invViewProj, float4(uv * 2.0 - 1.0, deviceDepth, 1.0));
        sceneDistance = length(receiverH.xyz / receiverH.w - params.cameraCoverage.xyz);
    }

    float2 reducedSize = float2(params.frame.yz);
    float2 reducedPos = uv * reducedSize - 0.5;
    int2 base = int2(floor(reducedPos));
    float2 fraction = frac(reducedPos);
    float centerDepth = cloudReducedDepth.SampleLevel(uv, 0.0);
    float4 cloud = float4(0.0);
    float weightSum = 0.0;
    float depthNumerator = 0.0;
    float depthDenominator = 0.0;
    for (int y = 0; y < 2; ++y)
    {
        for (int x = 0; x < 2; ++x)
        {
            int2 pixel = clamp(base + int2(x, y), int2(0), int2(reducedSize) - 1);
            float2 tapUv = (float2(pixel) + 0.5) / reducedSize;
            float4 value = cloudReduced.SampleLevel(tapUv, 0.0);
            float frontDepth = cloudReducedDepth.SampleLevel(tapUv, 0.0);
            float2 axisWeight = 1.0 - abs(float2(x, y) - fraction);
            float spatialWeight = axisWeight.x * axisWeight.y;
            float depthWeight = exp(-abs(frontDepth - centerDepth) /
                max(centerDepth * 0.02, 4.0));
            float visibility = frontDepth < sceneDistance ? 1.0 : 0.0;
            float weight = spatialWeight * depthWeight * visibility;
            cloud += value * weight;
            weightSum += weight;
            float opacity = saturate(1.0 - value.a);
            depthNumerator += frontDepth * weight * opacity;
            depthDenominator += weight * opacity;
        }
    }
    cloud = weightSum > 1e-5 ? cloud / weightSum : float4(0.0, 0.0, 0.0, 1.0);
    float meanDepth = depthDenominator > 1e-5 ? depthNumerator / depthDenominator : sceneDistance;
    cloudFullDepth[tid.xy] = meanDepth;
    cloudFullColor[tid.xy] = float4(max(cloud.rgb, float3(0.0)), saturate(cloud.a));
}
