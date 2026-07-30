module lighting;

import lighting_common;
import global_gpu_data;
import sdf;
import giprobe;
__exported import coverage;
__exported import material_params;
import sky_sh;
import thin_sheet;

public struct VertexOutput
{
    public float4 position : SV_Position;
    public float3 worldPos : POSITION0;
    public float3 worldNormal : NORMAL;
    public float2 uv0 : TEXCOORD0;
    public float3 coverageAnchor : TEXCOORD1;
    public nointerpolation uint materialIndex : TEXCOORD2;
    /// The record's representation-crossfade word (zero outside a transition).
    public nointerpolation uint transition : TEXCOORD3;
};

struct Camera
{
    float4x4 viewProj;
    /// The bucket's command-slice base, which the mesh executor's `SV_DrawIndex` is relative to.
    uint sliceBase;
};
[[vk::push_constant]] Camera camera;

/// The bucket's command-slice base for the mesh executor.
public uint executorSliceBase()
{
    return camera.sliceBase;
}

// Partially bound + update-after-bind; sampled with NonUniformResourceIndex. Bounded, not a
// runtime array — the length matches the renderer's bindless texture capacity.
[[vk::binding(0, 0)]] public Sampler2D albedoTextures[1024];

[[vk::binding(0, 1)]] ConstantBuffer<LightGlobals> globals;

[[vk::binding(1, 1)]] StructuredBuffer<GpuLight> lights;

[[vk::binding(2, 1)]] StructuredBuffer<Cluster> clusters;

[[vk::binding(3, 1)]] ConstantBuffer<ClusterParams> clusterParams;

// The integrated `(inScatter, transmittance)` froxel grid, addressed by `(screenUV, W)` — the
// same exponential-Z mapping `height_fog.slang` inverts. Bindings 8/9/10 are the SDF-occluder
// list + GDF clipmap, declared by the `sdf` module.
[[vk::binding(11, 1)]] Sampler3D<float4> froxelIntegration;
[[vk::binding(12, 1)]] Sampler2DArray<float> cloudShadowMap;
// The virtual-shadow physical page atlas (immutable compare sampler), reached through the page
// table in the light UBO.
[[vk::binding(13, 1)]] Sampler2DShadow vsmAtlas;

// Deduplicated per-material params in the frame's global parameter arena, indexed by the
// resident material record's `parameterIndex`.
[[vk::binding(2, 2)]] public StructuredBuffer<MaterialParams> materialParams;

// Image-based lighting: raw-radiance sky SH, prefiltered specular cube, split-sum BRDF LUT.
// Sampled for the ambient term when globals.counts.z != 0.
[[vk::binding(0, 3)]] StructuredBuffer<float4> skyShCoefficients;
[[vk::binding(1, 3)]] SamplerCube prefilteredMap;
[[vk::binding(2, 3)]] Sampler2D brdfLut;
static const float IblPrefilterMaxMip = 4.0;

// Perceptual roughness → prefiltered-cube mip (Filament `perceptualRoughnessToLod`). The `x*(2-x)`
// curve matches the GGX lobe size per mip better than a linear `roughness*maxMip`, so near-mirror
// surfaces stay on the sharp low mips.
float prefilterLod(float roughness)
{
    return IblPrefilterMaxMip * roughness * (2.0 - roughness);
}

// Screen-space maps, all sampled by screen UV. AO (factor, 1 = open) modulates the
// indirect/ambient term (counts.w); contact shadows (factor, 1 = lit) darken the
// directional direct term (screenFlags.x); SSGI (rgba16f radiance) adds one-bounce indirect
// (screenFlags.y).
[[vk::binding(0, 4)]] Texture2D<float4> aoMap;
[[vk::binding(1, 4)]] Texture2D<float4> contactMap;
[[vk::binding(2, 4)]] Texture2D<float4> ssgiMap;
// Screen-space reflections: rgb = reflected radiance, a = hit confidence (extraFlags.x).
[[vk::binding(3, 4)]] Texture2D<float4> ssrMap;
// Previous frame's lit color, sampled by reprojecting an RT reflection hit (extraFlags.y).
[[vk::binding(4, 4)]] Texture2D<float4> prevColorMesh;
// Reduced-resolution, temporally-accumulated DFAO sky-visibility (r = [0,1] factor, 1 = open),
// occluding the analytic sky irradiance when sdfOcclusion.y != 0.
[[vk::binding(5, 4)]] Texture2D<float4> dfaoMap;
// Reduced-resolution, temporally-accumulated specular reflection-occlusion (r = [0,1] factor,
// 1 = open), occluding the reflected skybox when sdfOcclusion.y != 0.
[[vk::binding(6, 4)]] Texture2D<float4> speoccMap;
// The half-res indirect-diffuse resolve (DDGI cage + IBL diffuse × sky-visibility), sampled
// with a bilinear upsample rather than resolved per full-res fragment.
[[vk::binding(7, 4)]] Texture2D<float4> giIndirectMap;
[[vk::binding(8, 4)]] SamplerState screenLinearSampler;

// DDGI probe atlases (octahedral irradiance + distance moments) for multi-bounce indirect when
// screenFlags.z != 0.
[[vk::binding(0, 5)]] Sampler2D ddgiIrradiance;
[[vk::binding(1, 5)]] Sampler2D ddgiDistance;

// ReSTIR resolved direct-light radiance (screen UV), replacing the punctual loop when
// screenFlags.w != 0.
//
// ReSTIR rides the ray-tracing path, so this binding — and every ray-query binding below — is
// compiled only into the RT variant. A device without ray tracing loads the RT-off variant, whose
// PSO layout omits sets 6/7: the declared shader interface must match the layout exactly, which
// MoltenVK enforces.
#ifndef SAFFRON_NO_RT
[[vk::binding(0, 7)]] Sampler2D restirRadiance;
#endif

// Spatial reflection probes share the IBL set so the mesh layout stays within the
// maxBoundDescriptorSets budget. Every array slot is seeded with the global IBL cubes, so an
// unused slot resolves to the global environment.
struct ProbeMeta
{
    float4 originRadius;     // xyz = world origin, w = influence radius
    float4 extentIntensity;  // xyz = box half-extents, w = intensity
    uint4 flags;             // x = valid, y = boxProjection
};
[[vk::binding(3, 3)]] SamplerCube probeCubes[8];        // MaxReflectionProbes
[[vk::binding(4, 3)]] SamplerCube probeIrradiance[8];
[[vk::binding(5, 3)]] StructuredBuffer<ProbeMeta> probeMeta;

// Parallax-corrected cubemap: re-project the reflection ray R against the probe's influence
// box (origin +- extent), returning a sample direction relative to the probe center.
float3 boxProject(float3 worldPos, float3 R, float3 origin, float3 extent)
{
    float3 invR = 1.0 / R;
    float3 tMax = (origin + extent - worldPos) * invR;
    float3 tMin = (origin - extent - worldPos) * invR;
    float3 t = max(tMax, tMin);
    float d = min(min(t.x, t.y), t.z);
    return (worldPos + R * d) - origin;
}

// The ray-tracing TLAS for inline ray-query shadows (pointShadowMeta.z gates it).
#ifndef SAFFRON_NO_RT
[[vk::binding(0, 6)]] RaytracingAccelerationStructure rtScene;

/// Traces one TerminateOnFirstHit shadow ray toward `toLight` (a unit dir); 1 = lit, 0 = shadowed.
/// The TLAS is a parameter so the mesh forward path and the fog inject pass share one copy.
public float rayQueryShadow(RaytracingAccelerationStructure tlas, float3 worldPos, float3 toLight, float maxDist)
{
    RayDesc ray;
    ray.Origin = worldPos + toLight * 0.02;  // bias off the surface to avoid self-hit
    ray.Direction = toLight;
    ray.TMin = 0.0;
    ray.TMax = maxDist;
    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(tlas, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xFF, ray);
    while (q.Proceed())
    {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE
            && gpuSceneRayCandidateCovered(
                gpuSceneAddresses,
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

// Traces a reflection ray for the closest opaque hit; the hit distance (> 0), or -1 on a miss.
float rayQueryReflection(float3 worldPos, float3 dir, float maxDist)
{
    RayDesc ray;
    ray.Origin = worldPos + dir * 0.02;  // bias off the surface to avoid self-hit
    ray.Direction = dir;
    ray.TMin = 0.0;
    ray.TMax = maxDist;
    RayQuery<RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(rtScene, RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xFF, ray);
    // Opaque candidates auto-commit; a non-opaque one commits only once the canonical classifier
    // confirms its coverage, so cutout texels stay transparent to rays.
    while (q.Proceed())
    {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE
            && gpuSceneRayCandidateCovered(
                gpuSceneAddresses,
                albedoTextures,
                q.CandidateInstanceID(),
                q.CandidateGeometryIndex(),
                q.CandidatePrimitiveIndex(),
                q.CandidateTriangleBarycentrics()))
        {
            q.CommitNonOpaqueTriangleHit();
        }
    }
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT ? q.CommittedRayT() : -1.0;
}
#endif

/// The camera-centered DDGI probe clipmap, as this frame's light globals describe it.
DdgiVolume ddgiVolumeFromGlobals()
{
    DdgiVolume v;
    v.volumeMin = globals.ddgiVolumeMin;
    v.volumeExtent = globals.ddgiVolumeExtent;
    v.probeCount = globals.ddgiProbeCount;
    v.scrollBase = globals.ddgiScrollBase;
    v.eyePosition = globals.eyePosition;
    return v;
}

float3 fresnelSchlickRoughness(float cosT, float3 F0, float roughness)
{
    return F0 + (max(float3(1.0 - roughness), F0) - F0) * pow(1.0 - cosT, 5.0);
}

// The executor vertex tail: the persistent-scene instance world transform (cofactor normals, so
// non-uniform scale stays correct) plus the instance's stored wind sway.
public VertexOutput transformExecutorVertex(
    float3 local,
    float3 localNormal,
    float2 uv,
    GpuSceneInstanceGpuRecord instance,
    float3 windOffset,
    uint parameterIndex,
    uint transition)
{
    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, false, c0, c1, c2, c3);
    float3 world = c0.xyz * local.x + c1.xyz * local.y + c2.xyz * local.z + c3.xyz
        + windOffset;
    float3 n0 = cross(c1.xyz, c2.xyz);
    float3 n1 = cross(c2.xyz, c0.xyz);
    float3 n2 = cross(c0.xyz, c1.xyz);
    float3 worldNormal = n0 * localNormal.x + n1 * localNormal.y + n2 * localNormal.z;

    VertexOutput output;
    output.worldPos = world;
    output.position = mul(camera.viewProj, float4(world, 1.0));
    output.worldNormal = worldNormal;
    output.uv0 = uv;
    output.coverageAnchor = local;
    output.materialIndex = parameterIndex;
    output.transition = transition;
    return output;
}

static const float PI = 3.14159265;

float3 fresnelSchlick(float cosT, float3 F0)
{
    return F0 + (1.0 - F0) * pow(1.0 - cosT, 5.0);
}

float distributionGGX(float ndoth, float a)  // a = roughness^2
{
    float a2 = a * a;
    float d = ndoth * ndoth * (a2 - 1.0) + 1.0;
    return a2 / max(PI * d * d, 1e-5);
}

float visibilitySmithGGX(float ndotv, float ndotl, float a)  // height-correlated Smith
{
    float a2 = a * a;
    float v = ndotl * sqrt(ndotv * ndotv * (1.0 - a2) + a2);
    float l = ndotv * sqrt(ndotl * ndotl * (1.0 - a2) + a2);
    return 0.5 / max(v + l, 1e-5);
}

// Cook-Torrance metallic/roughness BRDF. `radiance` is the incoming light's
// color * intensity * attenuation * cone; the cosine and diffuse/PI are applied here.
float3 brdf(float3 n, float3 v, float3 l, float3 albedo, float metallic, float roughness, float3 radiance)
{
    float3 h = normalize(v + l);
    float ndotl = max(dot(n, l), 0.0);
    float ndotv = max(dot(n, v), 1e-4);
    float ndoth = max(dot(n, h), 0.0);
    float a = roughness * roughness;
    float3 F0 = lerp(float3(0.04), albedo, metallic);
    float3 F = fresnelSchlick(max(dot(h, v), 0.0), F0);
    float D = distributionGGX(ndoth, a);
    float Vis = visibilitySmithGGX(ndotv, ndotl, a);
    float3 spec = D * Vis * F;
    // Multi-scatter energy compensation: scale single-scatter GGX back toward unity by the
    // single-scatter directional albedo Ess = ab.x + ab.y (≈1 at low roughness, < 1 at high).
    float2 lutAb = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
    spec *= 1.0 + F0 * (1.0 / max(lutAb.x + lutAb.y, 1e-3) - 1.0);
    float3 kd = (1.0 - F) * (1.0 - metallic);
    float3 diff = kd * albedo / PI;
    return (diff + spec) * radiance * ndotl;
}

// The material seam: what `evalSurface` — the fixed glTF metallic-roughness body, or node-graph
// codegen — hands the shared lighting below.
public struct SurfaceData
{
    public float3 albedo;
    public float metallic;
    public float roughness;
    public float3 normal;     // world-space, normalized
    public float3 sheetNormal; // authored front-facing sheet normal
    public float3 emissive;
    public float occlusion;   // 1.0 = none; multiplies the ambient term
    public float opacity;
    public uint surfaceModel; // 0 standard, 1 thin sheet
    public float frontFace;   // 1 front, 0 back
    public float frontResponse;
    public float backResponse;
    public float thickness;
    public float3 absorption;
    public float3 transmission;
    public float energyLimit;
};

ThinSheetEnergyPartition thinSheetPartition(SurfaceData surface, bool incidentFront, float cosine)
{
    float response = incidentFront ? surface.frontResponse : surface.backResponse;
    return thinSheetEnergyPartition(
        response,
        surface.thickness,
        surface.absorption,
        surface.transmission,
        surface.energyLimit,
        cosine);
}

float3 surfaceDirect(
    SurfaceData surface,
    float3 n,
    float3 v,
    float3 l,
    float3 radiance)
{
    if (surface.surfaceModel == 0u)
    {
        return brdf(n, v, l, surface.albedo, surface.metallic, surface.roughness, radiance);
    }
    float3 sheetNormal = normalize(surface.sheetNormal);
    float viewCosine = dot(sheetNormal, v);
    float lightCosine = dot(sheetNormal, l);
    bool viewFront = viewCosine >= 0.0;
    bool lightFront = lightCosine >= 0.0;
    ThinSheetEnergyPartition partition =
        thinSheetPartition(surface, lightFront, abs(lightCosine));
    if (viewFront == lightFront)
    {
        return brdf(
            n,
            v,
            l,
            surface.albedo,
            0.0,
            surface.roughness,
            radiance) * partition.reflection;
    }
    return partition.transmission * radiance * abs(lightCosine) / PI;
}

// One punctual light's contribution at a world-space surface point. `lightIndex` lets the
// shadowed spot light apply its shadow-map visibility.
float3 punctual(GpuLight lt, uint lightIndex, float3 worldPos, float3 n, float3 v,
                SurfaceData surface)
{
    float3 toLight = lt.positionRange.xyz - worldPos;
    float dist = length(toLight);
    if (dist > lt.positionRange.w)
    {
        return float3(0.0);
    }
    float3 l = toLight / max(dist, 0.0001);
    float attenuation = distanceAttenuation(dist, lt.positionRange.w);

    float cone = 1.0;
    float shadow = 1.0;
    // The ray-traced path shadows every punctual light; the map paths below shadow only the one
    // spot and the one point light that own a shadow allocation.
#ifndef SAFFRON_NO_RT
    if (globals.pointShadowMeta.z != 0)
    {
        shadow = rayQueryShadow(rtScene, worldPos, l, dist);
    }
#endif
    if (lt.directionType.w > 0.5)
    {
        float3 spotDir = normalize(lt.directionType.xyz);
        float cosAngle = dot(spotDir, -l);
        cone = smoothstep(lt.spotCos.y, lt.spotCos.x, cosAngle);
        if (globals.pointShadowMeta.z == 0 && globals.spotShadow.y != 0 && lightIndex == globals.spotShadow.x)
        {
            shadow = vsmSampleSpot(worldPos, globals, vsmAtlas);
        }
    }
    else if (globals.pointShadowMeta.z == 0 && globals.pointShadowMeta.y != 0 && lightIndex == globals.pointShadowMeta.x)
    {
        shadow = vsmSamplePoint(worldPos, globals, vsmAtlas);
    }
    float3 radiance = lt.colorIntensity.rgb * lt.colorIntensity.a * attenuation * cone * shadow;
    return surfaceDirect(surface, n, v, l, radiance);
}

public struct MaterialInput
{
    public float3 worldPos;
    public float3 worldNormal;
    public float3 sheetNormal;
    public float3 viewDir;   // world-space, toward the eye (for parallax)
    public float2 uv0;
    public MaterialParams mat;
};

// Per-fragment tangent frame from screen-space derivatives (no stored tangents needed). Schüler's
// method: solve for the tangent/bitangent that map the UV gradients onto the world-position gradients.
public float3x3 cotangentFrame(float3 N, float3 p, float2 uv)
{
    float3 dp1 = ddx(p);
    float3 dp2 = ddy(p);
    float2 duv1 = ddx(uv);
    float2 duv2 = ddy(uv);
    float3 dp2perp = cross(dp2, N);
    float3 dp1perp = cross(N, dp1);
    float3 T = dp2perp * duv1.x + dp1perp * duv2.x;
    float3 B = dp2perp * duv1.y + dp1perp * duv2.y;
    float invmax = rsqrt(max(dot(T, T), dot(B, B)));
    return float3x3(T * invmax, B * invmax, N);
}

// Parallax occlusion mapping: march the tangent-space view ray through the height field and
// return the offset UV. Height map: white = high; vt = tangent-space view dir (toward eye).
//
// Two measures keep the surface from swimming as the view grazes it and vt.z → 0:
//   * Offset limiting (Welsh 2003): march the *bounded* offset `v.xy * scale`, never divided by
//     v.z, so total UV travel stays ≤ scale at any angle. The trade is mild grazing flattening.
//   * Dynamic sample count (Tatarchuk 2006): more layers at grazing, where the ray is longest.
//     N·V is v.z in the tangent frame.
public float2 parallaxUv(float2 uv, float3 vt, uint heightIndex, float scale)
{
    float3 v = normalize(vt);
    const float MIN_STEPS = 8.0;
    const float MAX_STEPS = 32.0;
    const int MAX_LOOP = 32;
    float steps = lerp(MAX_STEPS, MIN_STEPS, saturate(v.z));
    float layerDepth = 1.0 / steps;
    float2 delta = v.xy * scale * layerDepth;
    float curDepth = 0.0;
    float2 cur = uv;
    float h = 1.0 - albedoTextures[NonUniformResourceIndex(heightIndex)].SampleLevel(cur, 0.0).r;
    for (int i = 0; i < MAX_LOOP; i = i + 1)
    {
        if (float(i) >= steps || curDepth >= h)
        {
            break;
        }
        cur = cur - delta;
        h = 1.0 - albedoTextures[NonUniformResourceIndex(heightIndex)].SampleLevel(cur, 0.0).r;
        curDepth = curDepth + layerDepth;
    }
    float2 prev = cur + delta;
    float afterH = h - curDepth;
    float beforeH = (1.0 - albedoTextures[NonUniformResourceIndex(heightIndex)].SampleLevel(prev, 0.0).r) - curDepth + layerDepth;
    float denom = afterH - beforeH;
    float w = abs(denom) > 1e-5 ? afterH / denom : 0.0;
    return lerp(cur, prev, clamp(w, 0.0, 1.0));
}

public MaterialInput makeMaterialInput(VertexOutput input)
{
    MaterialParams mp = materialParams[input.materialIndex];
    MaterialInput mi;
    mi.worldPos = input.worldPos;
    mi.worldNormal = input.worldNormal;
    mi.sheetNormal = input.worldNormal;
    mi.viewDir = normalize(globals.eyePosition.xyz - input.worldPos);
    mi.uv0 = input.uv0;
    mi.mat = mp;
    return mi;
}

/// The debug render-output channel (pointShadowMeta.w); 0 = full shading. `evalViewMode` holds the
/// channel → view-mode mapping, which mirrors `ViewMode::debug_channel`.
public uint debugViewChannel()
{
    return globals.pointShadowMeta.w;
}

// Jet-style heatmap ramp (blue → cyan → green → yellow → red) for a [0,1] scalar.
float3 heatmap(float t)
{
    t = saturate(t);
    float r = saturate(min(4.0 * t - 1.5, -4.0 * t + 4.5));
    float g = saturate(min(4.0 * t - 0.5, -4.0 * t + 3.5));
    float b = saturate(min(4.0 * t + 0.5, -4.0 * t + 2.5));
    return float3(r, g, b);
}

// The shared lighting half: a SurfaceData into lit radiance (direct + punctual + IBL/GI).
//
// `translucent` gates off every screen-space term — contact shadows, the screen-space GI resolve,
// GTAO, SSR, specular sky-occlusion, SSGI, and the ReSTIR direct sample — because a blend
// fragment's screen UV addresses the opaque geometry behind it, not the fragment. World-space
// indirect (analytic IBL irradiance + world-space DDGI) stands in.
public float4 evalLighting(VertexOutput input, SurfaceData surf, bool translucent)
{
    float3 n = surf.normal;
    float3 albedo = surf.albedo;

    float metallic = surf.metallic;
    float roughness = surf.roughness;
    float3 v = normalize(globals.eyePosition.xyz - input.worldPos);

    float2 screenUv = input.position.xy / float2(clusterParams.screenSize.xy);

    float3 lDir = -normalize(globals.directionAmbient.xyz);
    float shadow = 1.0;
#ifndef SAFFRON_NO_RT
    if (globals.pointShadowMeta.z != 0)
    {
        shadow = rayQueryShadow(rtScene, input.worldPos, lDir, 1e4);
    }
    else if (globals.counts.y != 0)
    {
        shadow = vsmSampleDirectional(input.worldPos, globals, vsmAtlas);
    }
#else
    if (globals.counts.y != 0)
    {
        shadow = vsmSampleDirectional(input.worldPos, globals, vsmAtlas);
    }
#endif
    if (globals.screenFlags.x != 0 && !translucent)
    {
        shadow *= contactMap.SampleLevel(screenLinearSampler, screenUv, 0.0).r;
    }
    shadow *= cloudShadowVisibility(
        input.worldPos, cloudShadowMap, globals, globals.cloudShadowMeta.z);
    float3 lo = surfaceDirect(
        surf,
        n,
        v,
        lDir,
        globals.colorIntensity.rgb * globals.colorIntensity.a) * shadow;
    float3 moonDir = -normalize(globals.moonDirectionIntensity.xyz);
    lo += surfaceDirect(
        surf,
        n,
        v,
        moonDir,
        globals.moonColor.rgb * globals.moonDirectionIntensity.w);

    // Punctual lights: ReSTIR's one resolved stochastic many-light sample (screenFlags.w), else
    // the froxel cluster's lights (clustered) or every light (reference).
#ifndef SAFFRON_NO_RT
    if (globals.screenFlags.w != 0 && !translucent && surf.surfaceModel == SURFACE_STANDARD)
    {
        float3 restir = restirRadiance.SampleLevel(screenUv, 0.0).rgb;
        lo += restir * albedo / PI;  // resolved radiance already includes geometry * vis * W
    }
    else if (clusterParams.screenSize.z != 0)
#else
    if (clusterParams.screenSize.z != 0)
#endif
    {
        float viewZ = mul(clusterParams.view, float4(input.worldPos, 1.0)).z;
        uint clusterIndex = clusterIndexFor(clusterParams, input.position.xy, viewZ);
        uint count = clusters[clusterIndex].count;
        for (uint i = 0; i < count; i = i + 1)
        {
            uint li = clusters[clusterIndex].indices[i];
            lo += punctual(lights[li], li, input.worldPos, n, v, surf);
        }
    }
    else
    {
        for (uint i = 0; i < globals.counts.x; i = i + 1)
        {
            lo += punctual(lights[i], i, input.worldPos, n, v, surf);
        }
    }

    // Indirect (ambient): split-sum IBL when enabled, else the flat scalar fallback.
    float3 ambient;
    float3 reflectionSpec = float3(0.0);  // IBL specular term, surfaced by the Reflections view mode
    if (globals.counts.z != 0)
    {
        float3 F0 = lerp(float3(0.04), albedo, metallic);
        float ndotv = max(dot(n, v), 1e-4);
        float3 F = fresnelSchlickRoughness(ndotv, F0, roughness);
        float3 kd = (1.0 - F) * (1.0 - metallic);
        // Analytic IBL sky irradiance along the shading normal: the residual indirect diffuse
        // wherever the DDGI cage below has no coverage.
        float3 irradiance = skyShIrradiance(n, skyShCoefficients);
        float3 R = reflect(-v, n);
        float3 prefiltered = prefilteredMap.SampleLevel(R, prefilterLod(roughness)).rgb;

        // Reflection probes: the nearest covering probe blends its prefiltered specular +
        // irradiance over the global IBL by an edge-soft weight (1 at centre, 0 at the boundary).
        // ambientColor.w bit-packs the probe count, so read it back as a u32.
        uint probeCount = asuint(globals.ambientColor.w);
        int best = -1;
        float bestDist = 1e30;
        for (uint pi = 0; pi < probeCount; pi = pi + 1)
        {
            if (probeMeta[pi].flags.x == 0) { continue; }
            float radius = probeMeta[pi].originRadius.w;
            float d = distance(input.worldPos, probeMeta[pi].originRadius.xyz);
            if (d < radius && d < bestDist) { bestDist = d; best = int(pi); }
        }
        if (best >= 0)
        {
            ProbeMeta pm = probeMeta[best];
            float3 origin = pm.originRadius.xyz;
            float radius = pm.originRadius.w;
            float3 Rp = R;
            if (pm.flags.y != 0) { Rp = boxProject(input.worldPos, R, origin, pm.extentIntensity.xyz); }
            float3 probeSpec = probeCubes[best].SampleLevel(Rp, prefilterLod(roughness)).rgb;
            float3 probeIrr = probeIrradiance[best].SampleLevel(n, 0.0).rgb;
            // Edge-soft weight: full inside ~60% of the radius, ramping to 0 at the boundary.
            float w = saturate(1.0 - (bestDist / radius - 0.6) / 0.4) * pm.extentIntensity.w;
            prefiltered = lerp(prefiltered, probeSpec, w);
            irradiance = lerp(irradiance, probeIrr, w);
        }

        // Screen-space reflections replace the prefiltered-env radiance where they hit, weighted
        // by hit confidence × a smoothness gate so rough surfaces keep the cube fallback. Applied
        // at the radiance level, so the BRDF, multi-scatter, horizon, and specular-AO below all
        // treat it uniformly.
        if (globals.extraFlags.x != 0 && !translucent)
        {
            float4 ssr = ssrMap.SampleLevel(screenLinearSampler, screenUv, 0.0);
            float gate = saturate(1.0 - roughness * 2.0);
            prefiltered = lerp(prefiltered, ssr.rgb, ssr.a * gate);
        }

        // Ray-traced reflections reach geometry off-screen, where SSR fails: trace against the
        // TLAS, then reproject the world-space hit through last frame's view-proj into prev_color
        // for its radiance. An off-screen reprojection or a miss falls back to the SSR/env value.
#ifndef SAFFRON_NO_RT
        if (globals.extraFlags.y != 0 && !translucent)
        {
            float gate = saturate(1.0 - roughness * 2.0);
            if (gate > 0.0)
            {
                float t = rayQueryReflection(input.worldPos, R, 100.0);
                if (t > 0.0)
                {
                    float3 hitPos = input.worldPos + R * t;
                    float4 hclip = mul(globals.prevViewProj, float4(hitPos, 1.0));
                    if (hclip.w > 0.0)
                    {
                        float2 huv = (hclip.xy / hclip.w) * 0.5 + 0.5;
                        if (all(huv >= float2(0.0)) && all(huv <= float2(1.0)))
                        {
                            float3 rtRefl = prevColorMesh.SampleLevel(screenLinearSampler, huv, 0.0).rgb;
                            prefiltered = lerp(prefiltered, rtRefl, gate);
                        }
                    }
                }
            }
        }
#endif

        // Specular sky occlusion: the reflected skybox is occluded only where the reflection
        // direction is blocked, never by the diffuse AO scalar — that would wrongly dim a chrome
        // surface facing open sky. The roughness-widened cone march runs in the specocc prepass;
        // this samples its resolved map. 1.0 when the pass is off this frame.
        float specSkyVis = (globals.sdfOcclusion.y != 0 && !translucent) ? speoccMap.SampleLevel(screenLinearSampler, screenUv, 0.0).r : 1.0;

        float2 ab = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
        float3 specularIBL = prefiltered * (F0 * ab.x + ab.y);
        // Multi-scatter energy compensation (Fdez-Aguera / Filament 4.7.2): single-scatter GGX
        // loses energy as roughness rises (a rough metal darkens up to ~60% at roughness 1). `Ess`
        // is the single-scatter directional albedo, the white-F0 reflectance ab.x + ab.y — ~1 at
        // low roughness, < 1 at high. `ab.y` alone explodes as it approaches 0 on smooth metals.
        float Ess = ab.x + ab.y;
        float3 energyComp = 1.0 + F0 * (1.0 / max(Ess, 1e-3) - 1.0);
        specularIBL *= energyComp;
        // Horizon occlusion: a reflection ray pointing below the geometric surface cannot see the
        // environment, or a curved/normal-mapped metal leaks reflections from under itself at
        // grazing angles. Uses the GEOMETRIC vertex normal, not the shading n.
        float horizon = min(1.0 + dot(R, normalize(input.worldNormal)), 1.0);
        specularIBL *= horizon * horizon;
        // Occlusion band split: large-range occlusion of indirect diffuse rides the DDGI ray-miss
        // below, so `ao` here is the contact-scale term only — material occlusion × small-radius
        // GTAO. It drives the roughness-aware specular-AO (Lagarde, → 1 on smooth metals so a
        // chrome sphere keeps its reflection in creases), the diffuse darkening, and SSGI.
        float ao = surf.occlusion;  // material occlusion (1.0 = none)
        if (globals.counts.w != 0 && !translucent)
        {
            ao *= aoMap.SampleLevel(screenLinearSampler, screenUv, 0.0).r;
        }
        float specAO = saturate(pow(ndotv + ao, exp2(-16.0 * roughness - 1.0)) - 1.0 + ao);
        specularIBL *= specAO * specSkyVis;  // reflection-cone occludes the reflected skybox
        reflectionSpec = specularIBL;

        // Indirect diffuse irradiance — the DDGI cage replacing the analytic IBL diffuse by
        // coverage, both occluded by the DFAO sky-visibility — is view-independent, so
        // `gi_resolve.slang` resolves it once per half-res pixel. This samples the bilinearly
        // upsampled result and applies the per-pixel kd·albedo tail.
        float3 indirectIrr;
        if (translucent)
        {
            // The GI resolve is keyed to the opaque G-buffer behind a translucent fragment, so
            // take its indirect diffuse from world-space sources instead.
            indirectIrr = irradiance;
            if (globals.screenFlags.z != 0)
            {
                float4 ddgi = ddgiSampleIrradiance(
                    ddgiVolumeFromGlobals(), ddgiIrradiance, ddgiDistance, input.worldPos, n);
                indirectIrr = lerp(irradiance, ddgi.rgb, ddgi.w);
            }
        }
        else
        {
            indirectIrr = giIndirectMap.SampleLevel(screenLinearSampler, screenUv, 0.0).rgb;
        }
        float3 indirect = kd * indirectIrr * albedo;
        // The large-range diffuse occlusion is already folded into indirectIrr by the DDGI
        // ray-miss, so `ao` adds only the contact detail the coarse probe grid cannot resolve —
        // no large-range double-count.
        ambient = (indirect * ao + specularIBL) * globals.iblTint.rgb;
        // SSGI is a bounce, not the sky term, so it adds on top — modulated by AO so occluded
        // creases do not over-bounce.
        if (globals.screenFlags.y != 0 && !translucent)
        {
            float3 gi = ssgiMap.SampleLevel(screenLinearSampler, screenUv, 0.0).rgb;
            ambient += gi * albedo * (1.0 - metallic) * ao;
        }
    }
    else
    {
        float ao = (globals.counts.w != 0 && !translucent) ? aoMap.SampleLevel(screenLinearSampler, screenUv, 0.0).r : 1.0;
        ambient = albedo * (1.0 - metallic) * globals.ambientColor.rgb * surf.occlusion * ao;
        if (globals.screenFlags.y != 0 && !translucent)
        {
            float3 gi = ssgiMap.SampleLevel(screenLinearSampler, screenUv, 0.0).rgb;
            ambient += gi * albedo * (1.0 - metallic) * ao;
        }
        // No analytic IBL diffuse to replace on this path, so the DDGI cage irradiance adds
        // directly, weighted by coverage to avoid a hard edge at the cage boundary.
        if (globals.screenFlags.z != 0)
        {
            float4 ddgi = ddgiSampleIrradiance(
                ddgiVolumeFromGlobals(), ddgiIrradiance, ddgiDistance, input.worldPos, n);
            ambient += ddgi.rgb * ddgi.w * albedo * (1.0 - metallic);
        }
    }
    if (surf.surfaceModel == SURFACE_THIN_SHEET)
    {
        bool viewFront = dot(normalize(surf.sheetNormal), v) >= 0.0;
        ThinSheetEnergyPartition reflectionPartition =
            thinSheetPartition(surf, viewFront, 1.0);
        ThinSheetEnergyPartition transmissionPartition =
            thinSheetPartition(surf, !viewFront, 1.0);
        float3 oppositeIrradiance;
        if (globals.counts.z != 0)
        {
            oppositeIrradiance = skyShIrradiance(-n, skyShCoefficients);
            if (globals.screenFlags.z != 0)
            {
                float4 oppositeDdgi = ddgiSampleIrradiance(
                    ddgiVolumeFromGlobals(),
                    ddgiIrradiance,
                    ddgiDistance,
                    input.worldPos,
                    -n);
                oppositeIrradiance = lerp(oppositeIrradiance, oppositeDdgi.rgb, oppositeDdgi.w);
            }
        }
        else
        {
            oppositeIrradiance = globals.ambientColor.rgb;
        }
        ambient = ambient * reflectionPartition.reflection
            + oppositeIrradiance * transmissionPartition.transmission / PI;
        reflectionSpec *= reflectionPartition.reflection;
    }

    // Two view modes peel a single term off the composed result.
    uint channel = debugViewChannel();
    if (channel == 9u)  // Reflections: IBL specular only
    {
        return float4(reflectionSpec, surf.opacity);
    }
    if (channel == 12u)  // GI: indirect/ambient only
    {
        return float4(ambient, surf.opacity);
    }
    float3 color = lo + ambient + surf.emissive;
    return float4(color, surf.opacity);
}

// Froxel volumetric fog for the forward transparent path: sample the integrated `(inScatter,
// transmittance)` volume at the fragment's screen UV + exponential-Z W, then apply
// `color*T + inScatter`. Transparents are not in the depth prepass, so the depth-based composite
// would fog them at the opaque depth behind them; this fogs them at their own distance.
float3 applyFroxelFog(float3 worldPos, float2 pixel, float3 color)
{
    if (globals.froxelFog.x < 0.5)
    {
        return color;
    }
    float near = globals.froxelFog.y;
    float far = globals.froxelFog.z;
    float2 uv = pixel / float2(clusterParams.screenSize.xy);
    // View-space forward distance (camera looks down -Z), clamped to the froxel near plane.
    float viewZ = max(-mul(clusterParams.view, float4(worldPos, 1.0)).z, near);
    float w = saturate(log(viewZ / near) / log(far / near));
    float4 vol = froxelIntegration.SampleLevel(float3(uv, w), 0.0);
    return color * vol.a + vol.rgb;
}

// Resolves the active view mode to a final color. Channel 0 is full shading, as is `kUnlit` (the
// per-material unlit PSO permutation) apart from its unlit output. Pure surface and screen-space
// channels return here; material-override modes recolour the surface and fall through to
// evalLighting, which peels off the Reflections/GI terms itself.
public float4 evalViewMode(VertexOutput input, SurfaceData surf, bool kUnlit, bool translucent)
{
    uint channel = debugViewChannel();

    // Pure surface-channel outputs (no lighting).
    if (channel == 1u) { return float4(surf.albedo, surf.opacity); }
    if (channel == 2u) { return float4(surf.normal * 0.5 + 0.5, surf.opacity); }
    if (channel == 3u) { return float4(surf.roughness.xxx, surf.opacity); }
    if (channel == 4u) { return float4(surf.metallic.xxx, surf.opacity); }
    if (channel == 5u) { return float4(surf.emissive, surf.opacity); }
    if (channel == 6u || kUnlit) { return float4(surf.albedo + surf.emissive, surf.opacity); }

    // Linearized view-space depth, normalized over the cluster z-range.
    if (channel == 10u)
    {
        float viewZ = mul(clusterParams.view, float4(input.worldPos, 1.0)).z;
        float near = clusterParams.zPlanes.x;
        float far = clusterParams.zPlanes.y;
        float d = saturate((max(-viewZ, near) - near) / max(far - near, 1e-3));
        return float4(d.xxx, 1.0);
    }

    // Screen-space ambient occlusion factor (white when SSAO is off).
    if (channel == 11u)
    {
        float2 screenUv = input.position.xy / float2(clusterParams.screenSize.xy);
        float ao = globals.counts.w != 0 ? aoMap.SampleLevel(screenLinearSampler, screenUv, 0.0).r : 1.0;
        return float4(ao.xxx, 1.0);
    }

    // Punctual-light complexity as a heatmap: 0 reads near-black, the ramp saturating to red
    // around 8 overlapping lights. Directional lights are not clustered, so they never count.
    if (channel == 13u)
    {
        uint count;
        if (clusterParams.screenSize.z != 0)
        {
            // Clustered: the froxel's assigned light count (lights whose sphere hits the froxel).
            float viewZ = mul(clusterParams.view, float4(input.worldPos, 1.0)).z;
            count = clusters[clusterIndexFor(clusterParams, input.position.xy, viewZ)].count;
        }
        else
        {
            // Matches the reference lighting loop, so the mode stays meaningful with clustering off.
            count = 0u;
            for (uint i = 0u; i < globals.counts.x; i = i + 1u)
            {
                GpuLight lt = lights[i];
                if (length(lt.positionRange.xyz - input.worldPos) <= lt.positionRange.w)
                {
                    count += 1u;
                }
            }
        }
        if (count == 0u)
        {
            return float4(float3(0.02), 1.0);
        }
        return float4(heatmap(saturate(float(count) / 8.0)), 1.0);
    }

    // Virtual-shadow pages: the directional sampler's resolved (level, page) as a stable colour,
    // dimmed where no level holds a resident page.
    if (channel == 14u)
    {
        if (globals.vsmPageTable == 0 || globals.vsmParams.z == 0.0)
        {
            return float4(0.05, 0.05, 0.05, 1.0);
        }
        float3 lightPos = mul(globals.vsmBasis, float4(input.worldPos, 1.0)).xyz;
        for (uint level = 0u; level < 8u; ++level)
        {
            float4 window = globals.vsmLevels[level];
            if (window.z <= 0.0)
            {
                continue;
            }
            float2 local = (lightPos.xy - window.xy) / window.z;
            if (local.x < 0.0 || local.y < 0.0 || local.x >= 1.0 || local.y >= 1.0)
            {
                continue;
            }
            uint2 page = uint2(local * 32.0);
            uint entry = *(uint*)(
                globals.vsmPageTable
                + (uint64_t(level) * 1024u + page.y * 32u + page.x) * 4u);
            if ((entry & VSM_TABLE_RESIDENT) == 0u)
            {
                continue;
            }
            uint hash = (level * 1024u + page.y * 32u + page.x) * 0x9e3779b9u;
            float3 colour = float3(
                float((hash >> 16u) & 0xffu) / 255.0,
                float((hash >> 8u) & 0xffu) / 255.0,
                float(hash & 0xffu) / 255.0);
            // Level tint: fine levels warm, coarse levels cool.
            float tint = float(level) / 7.0;
            colour = lerp(colour * float3(1.0, 0.8, 0.6), colour * float3(0.6, 0.8, 1.0), tint);
            return float4(colour * 0.85 + 0.15, 1.0);
        }
        return float4(0.05, 0.05, 0.05, 1.0);
    }

    // Material-override modes reuse the full lighting path on a recoloured surface.
    if (channel == 7u)  // Detail Lighting: neutral grey, normals preserved
    {
        surf.albedo = float3(0.18);
        surf.metallic = 0.0;
        surf.emissive = float3(0.0);
    }
    else if (channel == 8u)  // Lighting Only: flat white diffuse, geometric normal
    {
        surf.albedo = float3(1.0);
        surf.normal = normalize(input.worldNormal);
        surf.metallic = 0.0;
        surf.roughness = 1.0;
        surf.emissive = float3(0.0);
    }
    else if (channel == 9u)  // Reflections: mirror-like (evalLighting returns the specular term)
    {
        surf.metallic = 1.0;
        surf.roughness = 0.02;
    }

    float4 lit = evalLighting(input, surf, translucent);
    // Full shading only: the debug channels above are already display-referred and must not fog.
    if (translucent && channel == 0u)
    {
        lit.rgb = applyFroxelFog(input.worldPos, input.position.xy, lit.rgb);
    }
    return lit;
}
