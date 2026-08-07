// Joint-bilateral denoise + 2x upsample of the half-res GTAO map.
// Taps the source on its own texel grid — one sample per distinct source texel — and weights each
// against the full-res centre depth so AO does not bleed across depth discontinuities.
//
// The raw map carries the view-Z it was traced at in `g`, so a tap's depth rides the sample that
// already fetched it. Set 0: 0 = raw AO sampler (rg16f), 1 = G-buffer sampler, 2 = denoised AO
// storage (r8).

[[vk::binding(0, 0)]] Sampler2D aoRaw;
[[vk::binding(1, 0)]] Sampler2D gbuffer;
[[vk::image_format("r8")]]
[[vk::binding(2, 0)]] RWTexture2D<float> aoOut;

// exp(-d^2/4) over the full-res distance each source-grid step covers (2 full-res texels):
// centre 1, edge exp(-1), corner exp(-2).
static const float SPATIAL[9] = {
    0.13533528, 0.36787944, 0.13533528,
    0.36787944, 1.00000000, 0.36787944,
    0.13533528, 0.36787944, 0.13533528,
};

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
    uint srcWidth;
    uint srcHeight;
    aoRaw.GetDimensions(srcWidth, srcHeight);
    float2 srcTexel = 1.0 / float2(srcWidth, srcHeight);
    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);
    float centerZ = gbuffer.SampleLevel(uv, 0.0).a;

    float sum = 0.0;
    float weight = 0.0;
    [unroll]
    for (int y = -1; y <= 1; y = y + 1)
    {
        [unroll]
        for (int x = -1; x <= 1; x = x + 1)
        {
            float2 tap = aoRaw.SampleLevel(uv + float2(x, y) * srcTexel, 0.0).rg;
            float w = SPATIAL[(y + 1) * 3 + (x + 1)] * exp(-abs(tap.g - centerZ) * 4.0);
            sum += tap.r * w;
            weight += w;
        }
    }
    aoOut[tid.xy] = weight > 0.0 ? sum / weight : aoRaw.SampleLevel(uv, 0.0).r;
}
