// Joint-bilateral denoise + 2x upsample of a half-res rgba16f radiance/occlusion map — shared by
// the SSGI, DFAO, and specular-occlusion chains (bound with their own sets). One sample per
// distinct source texel, weighted against the full-res centre depth so indirect light does not
// bleed across depth discontinuities.
//
// Each source carries the view-Z it was traced at in its own alpha, so a tap's depth rides the
// sample that already fetched it — the weight costs no second texture read, and it compares
// against the depth the sample was actually traced at rather than a full-res depth near it.
// Set 0: 0 = raw source sampler, 1 = G-buffer sampler, 2 = denoised storage (rgba16f).

[[vk::binding(0, 0)]] Sampler2D ssgiRaw;
[[vk::binding(1, 0)]] Sampler2D gbuffer;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 0)]] RWTexture2D<float4> ssgiOut;

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
    ssgiOut.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    uint srcWidth;
    uint srcHeight;
    ssgiRaw.GetDimensions(srcWidth, srcHeight);
    float2 srcTexel = 1.0 / float2(srcWidth, srcHeight);
    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);
    float centerZ = gbuffer.SampleLevel(uv, 0.0).a;

    float3 sum = float3(0.0);
    float weight = 0.0;
    [unroll]
    for (int y = -1; y <= 1; y = y + 1)
    {
        [unroll]
        for (int x = -1; x <= 1; x = x + 1)
        {
            float4 tap = ssgiRaw.SampleLevel(uv + float2(x, y) * srcTexel, 0.0);
            float w = SPATIAL[(y + 1) * 3 + (x + 1)] * exp(-abs(tap.a - centerZ) * 4.0);
            sum += tap.rgb * w;
            weight += w;
        }
    }
    float3 result = weight > 0.0 ? sum / weight : ssgiRaw.SampleLevel(uv, 0.0).rgb;
    // Carry this pixel's view-Z in the alpha channel (the mesh samples only .rgb). The temporal
    // accumulation reads it back through the motion vector for a per-pixel disocclusion test.
    ssgiOut[tid.xy] = float4(result, centerZ);
}
