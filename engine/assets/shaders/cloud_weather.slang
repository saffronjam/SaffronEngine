// Resolves procedural or painted authoring into the one weather map sampled by cloud density.
// A painted source is sampled here into the same image; density never branches on its origin.

import clouds;

[[vk::image_format("rgba8")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> weatherOutput;
[[vk::binding(1, 0)]] ConstantBuffer<CloudParams> params;
[[vk::binding(2, 0)]] Sampler2D<float4> paintedWeather;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    weatherOutput.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);
    float3 weather;
    if (params.weather.w > 0.5)
    {
        weather = paintedWeather.SampleLevel(uv, 0.0).rgb;
    }
    else
    {
        float broad = periodicPerlin(float3(uv * 4.0, 0.25), 4u) * 0.5 + 0.5;
        float detail = periodicPerlin(float3(uv * 16.0, 0.75), 16u) * 0.5 + 0.5;
        float coverage = saturate(broad * 0.8 + detail * 0.2);
        float typeVariation = periodicPerlin(float3(uv * 2.0, 0.5), 2u) * 0.25 + 0.25;
        weather = float3(coverage, params.layer.w, typeVariation);
    }
    weatherOutput[tid.xy] = float4(weather, 1.0);
}
