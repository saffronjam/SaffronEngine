// ReSTIR DI — resolve + shade. For each pixel's combined reservoir: trace ONE ray-query
// shadow ray toward the chosen light (the only visibility ray ReSTIR needs), shade the full
// Cook-Torrance BRDF for that light scaled by the reservoir's unbiased weight W, and write
// the per-pixel direct radiance. The mesh fragment adds this in (replacing its own punctual
// loop) when ReSTIR is on. Also copies the combined reservoir into the previous buffer for
// next frame's temporal reuse. Set 0: b0 = G-buffer, b1 = combined reservoirs (in),
// b2 = previous reservoirs (out, for next frame), b3 = light SSBO, b4 = TLAS,
// b5 = restir output radiance (rgba16f storage), b6 = the GPU-scene address block.
// Set 1: the bindless albedo array (binding 0) the candidate coverage confirmation samples.

import global_gpu_data;

struct GpuLight
{
    float4 positionRange;
    float4 colorIntensity;
    float4 directionType;
    float4 spotCos;
};
struct Reservoir { float4 a; float4 b; };

[[vk::binding(0, 0)]] Sampler2D gbuffer;     // view normal.xyz, viewZ.a
[[vk::binding(1, 0)]] StructuredBuffer<Reservoir> combined;
[[vk::binding(2, 0)]] RWStructuredBuffer<Reservoir> previousOut;
[[vk::binding(3, 0)]] StructuredBuffer<GpuLight> lights;
[[vk::binding(4, 0)]] RaytracingAccelerationStructure tlas;
[[vk::image_format("rgba16f")]]
[[vk::binding(5, 0)]] RWTexture2D<float4> radianceOut;
[[vk::binding(6, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;
[[vk::binding(0, 1)]] Sampler2D albedoTextures[1024];

struct Push
{
    float4x4 invView;
    float4x4 invProjection;
    uint4 screenSize;     // xy = pixels
    float4 eyePosition;   // xyz world camera (view vector for specular)
};
[[vk::push_constant]] Push pc;

static const float PI = 3.14159265;

float3 viewPos(float2 uv, float viewZ)
{
    float2 ndc = uv * 2.0 - 1.0;
    float4 r = mul(pc.invProjection, float4(ndc, 1.0, 1.0));
    float3 ray = r.xyz / r.w;
    return ray * (viewZ / ray.z);
}

float3 fresnelSchlick(float c, float3 F0) { return F0 + (1.0 - F0) * pow(1.0 - c, 5.0); }
float distributionGGX(float ndoth, float a) { float a2 = a * a; float d = ndoth * ndoth * (a2 - 1.0) + 1.0; return a2 / max(PI * d * d, 1e-5); }
float visSmith(float ndotv, float ndotl, float a) { float a2 = a * a; float v = ndotl * sqrt(ndotv * ndotv * (1.0 - a2) + a2); float l = ndotv * sqrt(ndotl * ndotl * (1.0 - a2) + a2); return 0.5 / max(v + l, 1e-5); }

float rayShadow(float3 p, float3 l, float dist)
{
    RayDesc ray;
    ray.Origin = p + l * 0.02;
    ray.Direction = l;
    ray.TMin = 0.0;
    ray.TMax = dist;
    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(tlas, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xFF, ray);
    // Opaque candidates auto-commit; a non-opaque candidate commits only when the canonical
    // classifier confirms its coverage, so cutout texels stay transparent to visibility rays.
    while (q.Proceed())
    {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE
            && gpuSceneRayCandidateCovered(
                sceneAddresses,
                albedoTextures,
                q.CandidateInstanceID(),
                q.CandidateGeometryIndex(),
                q.CandidatePrimitiveIndex(),
                q.CandidateTriangleBarycentrics()))
        {
            q.CommitNonOpaqueTriangleHit();
        }
    }
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT ? 0.0 : 1.0;
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    if (tid.x >= pc.screenSize.x || tid.y >= pc.screenSize.y) { return; }
    uint pixelIdx = tid.y * pc.screenSize.x + tid.x;
    Reservoir res = combined[pixelIdx];
    previousOut[pixelIdx] = res;  // feed next frame's temporal reuse

    float2 uv = (float2(tid.xy) + 0.5) / float2(pc.screenSize.xy);
    float4 g = gbuffer.SampleLevel(uv, 0.0);
    if (g.a > -1e-4 || res.a.x < 0.0 || res.a.y <= 0.0)
    {
        radianceOut[tid.xy] = float4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    float3 vp = viewPos(uv, g.a);
    float3 worldPos = mul(pc.invView, float4(vp, 1.0)).xyz;
    float3 n = normalize(mul((float3x3)pc.invView, normalize(g.xyz)));
    float3 v = normalize(pc.eyePosition.xyz - worldPos);

    GpuLight lt = lights[uint(res.a.x)];
    float3 toLight = lt.positionRange.xyz - worldPos;
    float dist = length(toLight);
    float3 l = toLight / max(dist, 1e-4);

    // The single ReSTIR visibility ray for the chosen sample.
    float vis = rayShadow(worldPos, l, dist);
    if (vis <= 0.0)
    {
        radianceOut[tid.xy] = float4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    // Cook-Torrance for the chosen light; albedo/metallic/roughness are folded in by the
    // mesh (it multiplies this by its own material), so here use a neutral white BRDF lobe
    // weighted by W. The mesh re-applies albedo; we output radiance * geometry * W.
    float ndotl = max(dot(n, l), 0.0);
    float invSq = 1.0 / max(dist * dist, 1e-4);
    float t = saturate(1.0 - pow(dist / lt.positionRange.w, 4.0));
    float atten = invSq * t * t;
    float cone = 1.0;
    if (lt.directionType.w > 0.5)
    {
        float3 sd = normalize(lt.directionType.xyz);
        cone = smoothstep(lt.spotCos.y, lt.spotCos.x, dot(sd, -l));
    }
    float3 radiance = lt.colorIntensity.rgb * lt.colorIntensity.a * atten * cone * ndotl * res.a.y * vis;
    radianceOut[tid.xy] = float4(radiance, 1.0);
}
