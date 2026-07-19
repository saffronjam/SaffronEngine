// One-shot 128^2 periodic curl field derived from a tileable Perlin potential.

import clouds;

[[vk::image_format("rgba8")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> output;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    output.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);
    float eps = 1.0 / float(width);
    float py0 = periodicPerlin(float3(uv.x, (uv.y - eps), 0.5) * 8.0, 8u);
    float py1 = periodicPerlin(float3(uv.x, (uv.y + eps), 0.5) * 8.0, 8u);
    float px0 = periodicPerlin(float3((uv.x - eps), uv.y, 0.5) * 8.0, 8u);
    float px1 = periodicPerlin(float3((uv.x + eps), uv.y, 0.5) * 8.0, 8u);
    float2 curl = normalize(float2(py1 - py0, -(px1 - px0)) + float2(1e-5, 0.0));
    output[tid.xy] = float4(curl * 0.5 + 0.5, 0.0, 1.0);
}
