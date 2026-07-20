// One-shot 32^3 Worley edge-erosion bake, channel-packed at three rising frequencies.

import clouds;

[[vk::image_format("rgba8")]]
[[vk::binding(0, 0)]] RWTexture3D<float4> output;

[shader("compute")]
[numthreads(8, 8, 8)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    uint depth;
    output.GetDimensions(width, height, depth);
    if (any(tid >= uint3(width, height, depth)))
    {
        return;
    }
    float3 uv = (float3(tid) + 0.5) / float3(width, height, depth);
    output[tid] = float4(
        1.0 - periodicWorley(uv * 4.0, 4u),
        1.0 - periodicWorley(uv * 8.0, 8u),
        1.0 - periodicWorley(uv * 16.0, 16u),
        1.0);
}
