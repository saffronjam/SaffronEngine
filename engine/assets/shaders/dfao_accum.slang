// Temporal accumulation of the (spatially denoised) DFAO sky-visibility. Reprojects the previous
// frame's DFAO history through the motion vector, rejects out-of-screen and disoccluded history,
// and exponentially blends (EMA) with this frame. Writes the result to BOTH the resolved map the
// mesh samples and the next-frame history. Runs only when TAA is on (it shares the motion target +
// history parity). Set 0: 0 current (sampler), 1 history (sampler), 2 motion (sampler), 3 resolved
// out (storage), 4 history-write out (storage).
//
// WHY no neighborhood clamp (unlike ssgi_accum): DFAO sky visibility is a rotating, low-frequency
// Monte-Carlo estimate — each frame the cone-ring is rotated, so the per-frame estimate carries
// rotation variance. A pure EMA averages that variance away and converges to the rotation-invariant
// mean, so a static surface settles (stable). A 3x3 neighborhood min/max clamp re-injects the
// current frame's rotation variance into the resolved output every frame, so a static surface never
// converges — it flickers ("spinner"). UE5's DFAO handles unstable pixels with a spatial fill of
// low-confidence samples, not a per-frame neighborhood clamp; averaging is the correct operator for
// this signal, so this kernel is a clamp-free EMA.

[[vk::binding(0, 0)]] Sampler2D current;
[[vk::binding(1, 0)]] Sampler2D history;
[[vk::binding(2, 0)]] Sampler2D motion;
[[vk::image_format("rgba16f")]]
[[vk::binding(3, 0)]] RWTexture2D<float4> outResolved;
[[vk::image_format("rgba16f")]]
[[vk::binding(4, 0)]] RWTexture2D<float4> outHistory;

struct Push
{
    float4 params;  // x = history weight (0..1), y = 1 if history is valid this frame
};
[[vk::push_constant]] Push push;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    outResolved.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 texel = 1.0 / float2(width, height);
    float2 uv = (float2(tid.xy) + 0.5) * texel;

    float4 curSample = current.SampleLevel(uv, 0.0);
    float3 cur = curSample.rgb;
    float curZ = curSample.a; // this frame's view-Z, carried in alpha by ssgi-blur

    float2 mv = motion.SampleLevel(uv, 0.0).xy;
    float2 histUv = uv + mv;
    float weight = push.params.x;
    // Disocclusion / first frame: reprojected outside the screen -> no valid history.
    if (push.params.y < 0.5 || histUv.x < 0.0 || histUv.x > 1.0 || histUv.y < 0.0 || histUv.y > 1.0)
    {
        weight = 0.0;
    }
    float4 hist4 = history.SampleLevel(histUv, 0.0);
    // Per-pixel disocclusion: the reprojected history carries the view-Z it was written at; if that
    // diverges from this pixel's view-Z the motion vector landed on a different surface (a newly
    // revealed edge), so reset rather than smear the occluder's sky visibility across the seam.
    float histZ = hist4.a;
    if (weight > 0.0 && abs(curZ - histZ) > 0.1 * max(abs(curZ), 0.01))
    {
        weight = 0.0;
    }
    float3 hist = hist4.rgb; // no neighborhood clamp — see header

    float3 result = lerp(cur, hist, weight);
    // Propagate this frame's view-Z so next frame's reprojection can run the same test.
    outResolved[tid.xy] = float4(result, curZ);
    outHistory[tid.xy] = float4(result, curZ);
}
