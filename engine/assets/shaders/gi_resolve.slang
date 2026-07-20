// Screen-space indirect-diffuse resolve (Lumen-style "final gather" placement) — computes the
// view-independent indirect DIFFUSE irradiance once per HALF-res pixel off the thin G-buffer, so the
// forward fragment stops resolving the DDGI 8-probe cage + IBL diffuse per full-res pixel and instead
// samples one upsampled indirect-diffuse map. Mirrors `lighting.slang`'s `indirectIrr` exactly:
//   analyticIrr = skyShIrradiance(n) * dfao sky-visibility
//   indirectIrr = DDGI on ? lerp(analyticIrr, ddgiSampleIrradiance(...), coverage) : analyticIrr
// The DDGI cage sample is the shared `giprobe::ddgiSampleIrradiance` (one copy, set 5 atlases). The
// output feeds a bilateral upsample + the fragment multiply (kd·albedo·ao) — specular stays full-res.
//
// One descriptor set (set 0) — a self-contained compute pass, since `giprobe` declares no bindings
// (the DDGI atlases are passed in): b0 G-buffer sampler (view-normal.xyz + view-Z.a), b1 half-res
// indirect-diffuse storage, b2 params UBO, b3 raw-radiance sky SH, b4 resolved dfao
// sky-visibility, b5/b6 the DDGI irradiance + distance-moment atlases.

import giprobe;
import sky_sh;

[[vk::binding(0, 0)]] Sampler2D gbuffer;
[[vk::image_format("rgba16f")]]
[[vk::binding(1, 0)]] RWTexture2D<float4> indirectOut;

struct GiParams
{
    float4x4 invProjection;  // clip -> view (reconstruct view position from the G-buffer depth)
    float4x4 invView;        // view -> world (take the reconstructed position/normal to world)
    DdgiVolume vol;          // the camera-centered probe clipmap placement this frame
    uint4 flags;             // x = DDGI enabled, y = sky-occlusion (dfao) enabled, zw reserved
};
[[vk::binding(2, 0)]] ConstantBuffer<GiParams> params;

[[vk::binding(3, 0)]] StructuredBuffer<float4> skyShCoefficients;
[[vk::binding(4, 0)]] Sampler2D dfaoMap;          // resolved half-res sky-visibility (r = [0,1])
[[vk::binding(5, 0)]] Sampler2D ddgiIrradiance;   // DDGI octahedral irradiance atlas
[[vk::binding(6, 0)]] Sampler2D ddgiDistance;     // DDGI distance-moment atlas

// View-space position of a pixel, from its stored view-Z + a clip-space ray (as `dfao.slang`).
float3 viewPosFromUv(float2 uv, float viewZ)
{
    float2 ndc = uv * 2.0 - 1.0;
    float4 r = mul(params.invProjection, float4(ndc, 1.0, 1.0));
    float3 ray = r.xyz / r.w;
    return ray * (viewZ / ray.z);
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    indirectOut.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);

    float4 g = gbuffer.SampleLevel(uv, 0.0);
    float viewZ = g.a;
    // Background (no geometry): no indirect diffuse. `.a` carries the view-Z the denoise chain reads.
    if (viewZ > -1e-4)
    {
        indirectOut[tid.xy] = float4(0.0, 0.0, 0.0, viewZ);
        return;
    }

    float3 viewPos = viewPosFromUv(uv, viewZ);
    float3 worldPos = mul(params.invView, float4(viewPos, 1.0)).xyz;
    float3 n = normalize(mul((float3x3)params.invView, normalize(g.xyz)));

    float3 irradiance = skyShIrradiance(n, skyShCoefficients);
    float skyVis = params.flags.y != 0 ? dfaoMap.SampleLevel(uv, 0.0).r : 1.0;
    float3 analyticIrr = irradiance * skyVis;  // occlude the analytic sky (DFAO)
    float3 indirectIrr = analyticIrr;          // residual where DDGI is absent
    if (params.flags.x != 0)
    {
        float4 ddgi = ddgiSampleIrradiance(params.vol, ddgiIrradiance, ddgiDistance, worldPos, n);
        indirectIrr = lerp(analyticIrr, ddgi.rgb, ddgi.w);
    }
    indirectOut[tid.xy] = float4(indirectIrr, viewZ);
}
