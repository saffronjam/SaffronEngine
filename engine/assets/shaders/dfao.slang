// Diffuse sky-visibility (DFAO) prepass — Wright, SIGGRAPH 2015 "Dynamic Occlusion with
// Signed Distance Fields". A reduced-resolution cone trace of the composited Global Distance
// Field over the hemisphere of each pixel's normal, reconstructing world position + normal
// from the thin G-buffer. The output feeds a bilateral upsample + temporal accumulation
// (the dfao-blur → dfao-accum chain, reusing the SSGI denoise kernels); the mesh then samples
// the resolved sky-visibility (set 4 binding 5) to occlude the analytic sky irradiance. The
// 9-cone GDF march is too expensive per full-res fragment, so it runs once per half-res pixel
// here and is denoised across frames.
//
// Sets: 0 = the bindless brick arrays (the `sdf` import; unused on this path — only the GDF
// clipmap is tapped), 1 = the light set (GDF cascade clipmap + params at bindings 9/10),
// 2 = this pass's I/O (b0 = G-buffer sampler carrying view-normal.xyz + view-Z.a, b1 = the
// half-res sky-visibility storage image). Push = the camera inverses + the frame index.

import sdf;

[[vk::binding(0, 2)]] Sampler2D gbuffer;
[[vk::image_format("rgba16f")]]
[[vk::binding(1, 2)]] RWTexture2D<float4> skyVisOut;

struct Push
{
    float4x4 invProjection;  // clip -> view (reconstruct view position from the G-buffer depth)
    float4x4 invView;        // view -> world (take the reconstructed position/normal to world)
    float4 params;           // x = frame index (cone azimuth jitter), yzw reserved
};
[[vk::push_constant]] Push pc;

static const float TWO_PI = 6.28318530718;

// View-space position of a pixel, from its stored view-Z + a clip-space ray.
float3 viewPosFromUv(float2 uv, float viewZ)
{
    float2 ndc = uv * 2.0 - 1.0;
    float4 r = mul(pc.invProjection, float4(ndc, 1.0, 1.0));
    float3 ray = r.xyz / r.w;
    return ray * (viewZ / ray.z);
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    skyVisOut.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 texel = 1.0 / float2(width, height);
    float2 uv = (float2(tid.xy) + 0.5) * texel;

    float4 g = gbuffer.SampleLevel(uv, 0.0);
    float viewZ = g.a;
    // Background (no geometry): fully open sky. The .a carries the view-Z the denoise chain
    // reads back for its depth-aware weighting.
    if (viewZ > -1e-4)
    {
        skyVisOut[tid.xy] = float4(1.0, 1.0, 1.0, viewZ);
        return;
    }

    float3 viewPos = viewPosFromUv(uv, viewZ);
    float3 worldPos = mul(pc.invView, float4(viewPos, 1.0)).xyz;
    float3 worldN = normalize(mul((float3x3)pc.invView, normalize(g.xyz)));

    // Per-frame azimuth jitter rotates the 9-cone hemisphere set so the temporal accumulator
    // supersamples the penumbra across frames (UE's DFAO bounded jitter set). A finite, repeating
    // 8-position cycle — NOT an unbounded golden-ratio rotation — so the accumulated set is
    // deterministic and the EMA converges to the exact same value every 8 frames on a static view
    // (an irrational never-repeating rotation is a moving target the resolve can never settle on).
    float jitter = (float(uint(pc.params.x) & 7u) / 8.0) * TWO_PI;
    float vis = sdfSkyVisibility(worldPos, worldN, jitter);
    // Replicate into rgb so the bilateral blur + neighborhood clamp (the shared SSGI kernels,
    // which weight the rgb channels) operate symmetrically; the mesh reads .r.
    skyVisOut[tid.xy] = float4(vis, vis, vis, viewZ);
}
