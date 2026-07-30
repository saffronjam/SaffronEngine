// Depth-aware (bilateral) 5x5 blur of the raw GTAO map.
// Weights neighbors by view-Z similarity (read from the G-buffer .a) so the blur does not
// bleed AO across depth discontinuities. Set 0: 0 = raw AO sampler, 1 = G-buffer sampler,
// 2 = denoised AO storage (r8).

[[vk::binding(0, 0)]] Sampler2D aoRaw;
[[vk::binding(1, 0)]] Sampler2D gbuffer;
[[vk::image_format("r8")]]
[[vk::binding(2, 0)]] RWTexture2D<float> aoOut;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    aoOut.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 texel = 1.0 / float2(width, height);
    float2 uv = (float2(tid.xy) + 0.5) * texel;
    float centerZ = gbuffer.SampleLevel(uv, 0.0).a;

    float sum = 0.0;
    float weight = 0.0;
    for (int y = -2; y <= 2; y = y + 1)
    {
        for (int x = -2; x <= 2; x = x + 1)
        {
            float2 suv = uv + float2(x, y) * texel;
            float z = gbuffer.SampleLevel(suv, 0.0).a;
            // Gaussian-ish spatial weight * a depth-similarity weight.
            float w = exp(-float(x * x + y * y) / 4.0);
            w *= exp(-abs(z - centerZ) * 4.0);
            sum += aoRaw.SampleLevel(suv, 0.0).r * w;
            weight += w;
        }
    }
    aoOut[tid.xy] = weight > 0.0 ? sum / weight : aoRaw.SampleLevel(uv, 0.0).r;
}
