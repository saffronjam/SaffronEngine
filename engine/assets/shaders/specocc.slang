// Specular reflection-occlusion prepass — the reflection-vector half of "Dynamic Occlusion
// with Signed Distance Fields" (Wright, SIGGRAPH 2015). A reduced-resolution cone trace of the
// composited Global Distance Field along each pixel's reflection vector, its half-angle widening
// with roughness, reconstructing world position + normal from the thin G-buffer. The output feeds
// a bilateral upsample + temporal accumulation (the specocc-blur → specocc-accum chain, reusing
// the SSGI denoise kernels); the mesh then samples the resolved occlusion (set 4 binding 6) to
// occlude the reflected skybox. This is why the reflection cone-march no longer runs per full-res
// fragment — it runs once per half-res pixel here and is denoised across frames.
//
// Sets: 0 = the bindless brick arrays (the `sdf` import; unused on this path — only the GDF
// clipmap is tapped), 1 = the light set (GDF cascade clipmap + params at bindings 9/10),
// 2 = this pass's I/O (b0 = G-buffer sampler carrying view-normal.xyz + view-Z.a, b1 = the
// per-pixel roughness sampler, b2 = the half-res occlusion storage image). Push = the camera
// inverses + the frame index.

import sdf;

[[vk::binding(0, 2)]] Sampler2D gbuffer;
[[vk::binding(1, 2)]] Sampler2D roughnessTex;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 2)]] RWTexture2D<float4> specOccOut;

struct Push
{
    float4x4 invProjection;  // clip -> view (reconstruct view position from the G-buffer depth)
    float4x4 invView;        // view -> world (take the reconstructed position/normal to world)
    float4 params;           // x = frame index (reserved), yzw reserved
};
[[vk::push_constant]] Push pc;

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
    specOccOut.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 texel = 1.0 / float2(width, height);
    float2 uv = (float2(tid.xy) + 0.5) * texel;

    float4 g = gbuffer.SampleLevel(uv, 0.0);
    float viewZ = g.a;
    // Background (no geometry): the reflection reaches open sky, fully unoccluded. The .a carries
    // the view-Z the denoise chain reads back for its depth-aware weighting.
    if (viewZ > -1e-4)
    {
        specOccOut[tid.xy] = float4(1.0, 1.0, 1.0, viewZ);
        return;
    }

    float3 viewPos = viewPosFromUv(uv, viewZ);
    float3 worldPos = mul(pc.invView, float4(viewPos, 1.0)).xyz;
    float3 worldN = normalize(mul((float3x3)pc.invView, normalize(g.xyz)));

    // Reflection vector from the camera through this pixel: the reflected skybox the cone occludes.
    float3 cameraPos = mul(pc.invView, float4(0.0, 0.0, 0.0, 1.0)).xyz;
    float3 viewDir = normalize(worldPos - cameraPos);
    float3 R = reflect(viewDir, worldN);
    float roughness = roughnessTex.SampleLevel(uv, 0.0).r;

    float occ = sdfReflectionOcclusion(worldPos, R, roughness);
    // Replicate into rgb so the bilateral blur + neighborhood clamp (the shared SSGI kernels,
    // which weight the rgb channels) operate symmetrically; the mesh reads .r.
    specOccOut[tid.xy] = float4(occ, occ, occ, viewZ);
}
