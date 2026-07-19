// Reprojects and reconstructs the quarter-area cloud accumulation. The 4x4 fresh phase is blended
// into the previous reduced history; disocclusions reset and a 3x3 bounds clamp suppresses ghosts.

import clouds;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> currentReduced;
[[vk::binding(1, 0)]] Sampler2D<float4> previousReduced;
[[vk::binding(2, 0)]] Sampler2D<float> reducedDepth;
[[vk::binding(3, 0)]] Sampler2D<float4> motionTex;
[[vk::binding(4, 0)]] ConstantBuffer<CloudParams> params;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    currentReduced.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 size = float2(width, height);
    float2 uv = (float2(tid.xy) + 0.5) / size;
    bool fresh = (((tid.x & 3u) | ((tid.y & 3u) << 2u)) == (params.frame.x & 15u));
    float4 current = currentReduced[tid.xy];
    if (params.march.w < 0.5)
    {
        currentReduced[tid.xy] = fresh ? current : float4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    float depth = reducedDepth.SampleLevel(uv, 0.0);
    float4 farH = mul(params.invViewProj, float4(uv * 2.0 - 1.0, 1.0, 1.0));
    float3 rayDirection = normalize(farH.xyz / farH.w - params.cameraCoverage.xyz);
    float3 world = params.cameraCoverage.xyz + rayDirection * depth;
    float4 previousClip = mul(params.prevViewProj, float4(world, 1.0));
    float2 previousUv = previousClip.xy / max(previousClip.w, 1e-6) * 0.5 + 0.5;
    float2 surfacePreviousUv = uv + motionTex.SampleLevel(uv, 0.0).xy;
    bool offscreen = previousClip.w <= 0.0 || any(previousUv < 0.0) || any(previousUv > 1.0);
    bool surfaceDisocclusion = length(surfacePreviousUv - previousUv) > 0.2;
    if (offscreen || surfaceDisocclusion)
    {
        currentReduced[tid.xy] = fresh ? current : float4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    float4 history = previousReduced.SampleLevel(previousUv, 0.0);
    if (!fresh)
    {
        currentReduced[tid.xy] = history;
        return;
    }

    float4 neighborhoodMin = current;
    float4 neighborhoodMax = current;
    for (int y = -1; y <= 1; ++y)
    {
        for (int x = -1; x <= 1; ++x)
        {
            int2 tap = clamp(int2(tid.xy) + int2(x, y), int2(0), int2(width - 1, height - 1));
            float4 value = currentReduced[tap];
            neighborhoodMin = min(neighborhoodMin, value);
            neighborhoodMax = max(neighborhoodMax, value);
        }
    }
    history = clamp(history, neighborhoodMin, neighborhoodMax);
    currentReduced[tid.xy] = lerp(history, current, saturate(params.march.z));
}
