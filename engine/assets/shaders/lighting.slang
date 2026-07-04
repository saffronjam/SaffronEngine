module lighting;

import sdf;
import giprobe;

// Instanced mesh shader. Vertex inputs come from an interleaved buffer
// (position/normal/uv0); per-instance model + normal matrix + base color come from
// a storage buffer (set 2) indexed by the absolute Vulkan InstanceIndex
// (firstInstance + instance). SV_VulkanInstanceID is that builtin — plain
// SV_InstanceID is base-relative (excludes firstInstance), which would break batching.
// The push constant is the camera viewProj. Set 0 is albedo; set 1 is the lighting set:
// directional UBO (0), punctual light list (1), per-cluster light lists (2), cluster
// params UBO (3). Punctual lights are clustered-forward: the fragment loops only its
// froxel cluster's lights (params.screenSize.z != 0), or every light otherwise.

static const uint MAX_LIGHTS_PER_CLUSTER = 64;

public struct VertexInput
{
    [[vk::location(0)]] public float3 position;
    [[vk::location(1)]] public float3 normal;
    [[vk::location(2)]] public float2 uv0;
};

// The skinned vertex layout: the base stream plus a second binding carrying the joint
// indices + blend weights (the skinned PSO's vertexMainSkinned consumes it; the base
// VertexInput and unskinned PSOs are untouched).
public struct SkinnedVertexInput
{
    [[vk::location(0)]] public float3 position;
    [[vk::location(1)]] public float3 normal;
    [[vk::location(2)]] public float2 uv0;
    [[vk::location(3)]] public uint4 joints;
    [[vk::location(4)]] public float4 weights;
};

public struct VertexOutput
{
    public float4 position : SV_Position;
    public float3 worldPos : POSITION0;
    public float3 worldNormal : NORMAL;
    public float2 uv0 : TEXCOORD0;
    public nointerpolation uint materialIndex : TEXCOORD1;  // index into materialParams (set 2, binding 2)
};

struct Camera
{
    float4x4 viewProj;
};
[[vk::push_constant]] Camera camera;

// Bindless: one global albedo array (set 0) indexed per-instance. Partially bound +
// update-after-bind on the C++ side; sampled with NonUniformResourceIndex. The size
// matches MaxBindlessTextures in renderer.cppm (a bounded array, not a runtime one).
[[vk::binding(0, 0)]] public Sampler2D albedoTextures[1024];

// The bindless per-mesh SDF array (set 0, binding 1) is declared by the shared `sdf` module
// (linked above); the specular reflection-occlusion cone trace below indexes it through that
// module's `sdfReflectionOcclusion`.

struct LightGlobals
{
    float4 directionAmbient;     // xyz = travel direction, w = ambient
    float4 colorIntensity;       // rgb = color, a = intensity
    uint4 counts;                // x = punctual count, y = directional shadow, z = IBL, w = SSAO
    float4 eyePosition;          // xyz = world-space camera position (for the view vector)
    float4x4 shadowViewProj;     // world -> directional light clip space
    float4x4 spotShadowViewProj; // world -> shadowed spot light clip space (perspective)
    uint4 spotShadow;            // x = shadowed spot's light index, y = enabled (0/1)
    float4 pointShadow;          // xyz = shadowed point light world pos, w = far plane
    uint4 pointShadowMeta;       // x = shadowed point's light index, y = enabled (0/1)
    uint4 screenFlags;           // x = contact shadows, y = SSGI, z = DDGI (AO is counts.w)
    float4 ddgiVolumeMin;        // xyz = DDGI probe volume world min
    float4 ddgiVolumeExtent;     // xyz = DDGI probe volume world size
    uint4 ddgiProbeCount;        // xyz = probes per axis, w = irradiance tile interior
    uint4 ddgiScrollBase;        // xyz = toroidal scroll base (wrapMod(snapBase, count)), w reserved
    uint4 sdfOcclusion;          // x reserved, y = GDF reflection-occlusion enabled (0/1), zw reserved
    float4 ambientColor;         // rgb = scene-environment ambient (non-IBL fallback)
    uint4 extraFlags;            // x = screen-space reflections, y = ray-traced reflections
    float4x4 prevViewProj;       // previous frame world->clip, reprojecting an RT hit into prev_color
};
[[vk::binding(0, 1)]] ConstantBuffer<LightGlobals> globals;

struct GpuLight
{
    float4 positionRange;   // xyz = world position, w = range
    float4 colorIntensity;  // rgb = color, a = intensity
    float4 directionType;   // xyz = world direction (spot), w = type (0 point, 1 spot)
    float4 spotCos;         // x = cos(inner half-angle), y = cos(outer half-angle)
};
[[vk::binding(1, 1)]] StructuredBuffer<GpuLight> lights;

struct Cluster
{
    uint count;
    uint indices[MAX_LIGHTS_PER_CLUSTER];
};
[[vk::binding(2, 1)]] StructuredBuffer<Cluster> clusters;

struct ClusterParams
{
    float4x4 view;
    float4x4 inverseProjection;
    uint4 gridSize;       // xyz = grid dims, w = light count
    uint4 screenSize;     // xy = pixels, z = clustered flag
    float4 zPlanes;       // x = near, y = far
};
[[vk::binding(3, 1)]] ConstantBuffer<ClusterParams> clusterParams;

// Directional + spot shadow maps: depth textures sampled with a comparison (PCF) sampler.
[[vk::binding(4, 1)]] Sampler2DShadow shadowMap;
[[vk::binding(5, 1)]] Sampler2DShadow spotShadowMap;
// Point shadow: two omnidirectional cubes of world distance-to-light (linear sampler) — a cached
// STATIC cube (non-deformed casters) and a per-frame DYNAMIC cube (skinned / morph casters). The
// nearest occluder is the min of the two, so a moving character composites over the cached
// environment without re-rendering it.
[[vk::binding(6, 1)]] SamplerCube pointShadowMap;
[[vk::binding(7, 1)]] SamplerCube pointShadowMapDynamic;

// The shared `sdf` module (linked above) also declares the per-mesh SDF-occluder instance list
// (set 1, binding 8) + the Global-SDF cascade clipmap (bindings 9/10). The fragment reaches only
// the GDF tap (the reflection-occlusion cone, `gdfDistance`); the instance list is used solely by
// the DDGI ray trace's near-field sphere-march, so it is dead-stripped from this fragment.

// Visibility for the shadowed point light: compare the fragment's distance to the light against the
// nearest occluder distance across both cubes (sampled by light->frag dir).
float pointShadow(float3 worldPos)
{
    float3 toFrag = worldPos - globals.pointShadow.xyz;
    float dist = length(toFrag);
    float3 dir = normalize(toFrag);
    float stored = min(
        pointShadowMap.SampleLevel(dir, 0.0).r,
        pointShadowMapDynamic.SampleLevel(dir, 0.0).r);
    const float bias = 0.08;  // matches PointShadowDistanceBias (renderer_detail.cppm)
    return dist - bias <= stored ? 1.0 : 0.0;
}

// 3x3 PCF visibility for a light-space projection (1 = lit, 0 = shadowed). Off-map and
// beyond-far-plane samples are treated as lit. `map` selects which shadow texture.
float pcfShadow(Sampler2DShadow map, float4x4 viewProj, float3 worldPos)
{
    float4 clip = mul(viewProj, float4(worldPos, 1.0));
    float3 ndc = clip.xyz / clip.w;
    float2 uv = ndc.xy * 0.5 + 0.5;
    if (clip.w <= 0.0 || uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z > 1.0)
    {
        return 1.0;
    }
    const float texel = 1.0 / 2048.0;  // matches ShadowMapSize
    float sum = 0.0;
    for (int x = -1; x <= 1; x = x + 1)
    {
        for (int y = -1; y <= 1; y = y + 1)
        {
            sum += map.SampleCmp(uv + float2(x, y) * texel, ndc.z);
        }
    }
    return sum / 9.0;
}

struct Instance
{
    float4x4 model;
    float4x4 normalMatrix;
    float4x4 prevModel;  // last frame's world matrix (TAA object-motion reprojection)
    float4 baseColor;
    uint4 texture;    // x = bindless albedo index
    float4 pbr;       // x = metallic, y = roughness
    float4 emissive;  // rgb = emissive radiance (strength baked in)
};
[[vk::binding(0, 2)]] StructuredBuffer<Instance> instances;
// The frame's joint palette (worldBone * inverseBind per joint); a skinned instance
// indexes it at texture.y + vertex joint index.
[[vk::binding(1, 2)]] StructuredBuffer<float4x4> jointMatrices;

// Set 2, binding 2: deduplicated per-material params, indexed by Instance.texture.w (the
// material index). Many instances of one material share one entry; the fragment reads its
// texture indices + factors from here. Layout mirrors MaterialParamsData (renderer_types.cppm).
public struct MaterialParams
{
    public float4 baseColor;
    public float4 pbr;       // x = metallic, y = roughness, z = normalStrength, w = alphaCutoff
    public float4 emissive;  // rgb = radiance, w = heightScale
    public float4 uv;        // tiling.xy, offset.xy
    public uint4 tex0;       // albedo, orm/mr, normal, emissive (bindless indices)
    public uint4 tex1;       // height, reserved, reserved, featureBits
};
[[vk::binding(2, 2)]] public StructuredBuffer<MaterialParams> materialParams;

// Set 3: image-based lighting — diffuse irradiance cube, prefiltered specular cube, and
// the split-sum BRDF LUT. Sampled for the ambient term when globals.counts.z != 0.
[[vk::binding(0, 3)]] SamplerCube irradianceMap;
[[vk::binding(1, 3)]] SamplerCube prefilteredMap;
[[vk::binding(2, 3)]] Sampler2D brdfLut;
static const float IblPrefilterMaxMip = 4.0;  // == IblPrefilterMips - 1 (renderer_detail.cppm)

// Perceptual roughness → prefiltered-cube mip (Filament `perceptualRoughnessToLod`). The `x*(2-x)`
// curve matches the GGX lobe size per mip better than a linear `roughness*maxMip`, so near-mirror
// surfaces stay on the sharp low mips and don't snap to a blurry level.
float prefilterLod(float roughness)
{
    return IblPrefilterMaxMip * roughness * (2.0 - roughness);
}

// Set 4: screen-space maps, all sampled by screen UV. AO (factor, 1 = open) modulates the
// indirect/ambient term (counts.w); contact shadows (factor, 1 = lit) darken the
// directional direct term (screenFlags.x); SSGI (rgba16f radiance) adds one-bounce indirect
// (screenFlags.y).
[[vk::binding(0, 4)]] Sampler2D aoMap;
[[vk::binding(1, 4)]] Sampler2D contactMap;
[[vk::binding(2, 4)]] Sampler2D ssgiMap;
// Screen-space reflection map (rgb = reflected radiance, a = hit confidence), blended over
// the prefiltered-env specular when extraFlags.x != 0.
[[vk::binding(3, 4)]] Sampler2D ssrMap;
// Previous frame's lit color, sampled by reprojecting an RT reflection hit (extraFlags.y).
[[vk::binding(4, 4)]] Sampler2D prevColorMesh;
// Reduced-resolution, temporally-accumulated DFAO sky-visibility (r = [0,1] factor, 1 = open),
// produced by the dfao → dfao-blur → dfao-accum prepass. Sampled to occlude the analytic sky
// irradiance when sdfOcclusion.y != 0 (the diffuse half of the DFAO term; the per-fragment
// 9-cone GDF march it replaces).
[[vk::binding(5, 4)]] Sampler2D dfaoMap;
// Reduced-resolution, temporally-accumulated specular reflection-occlusion (r = [0,1] factor,
// 1 = open), produced by the specocc → specocc-blur → specocc-accum prepass. Sampled to occlude
// the reflected skybox when sdfOcclusion.y != 0 (the specular half of the SDF occlusion term; the
// per-fragment reflection cone-march it replaces).
[[vk::binding(6, 4)]] Sampler2D speoccMap;
// The half-res screen-space indirect-diffuse resolve (DDGI cage + IBL diffuse × sky-visibility),
// computed once per half-res pixel by the gi-resolve pass; sampled here (bilinear upsample) instead
// of resolving the DDGI cage + IBL diffuse per full-res fragment.
[[vk::binding(7, 4)]] Sampler2D giIndirectMap;

// Set 5: DDGI probe atlases (octahedral irradiance + distance moments), passed to the shared
// `giprobe::ddgiSampleIrradiance` (which declares no bindings of its own) for multi-bounce indirect
// when screenFlags.z != 0.
[[vk::binding(0, 5)]] Sampler2D ddgiIrradiance;
[[vk::binding(1, 5)]] Sampler2D ddgiDistance;

// Set 7: ReSTIR resolved direct-light radiance (screen UV). Replaces the punctual light
// loop when screenFlags.w != 0 (multiplied by albedo/PI for the diffuse direct term).
[[vk::binding(0, 7)]] Sampler2D restirRadiance;

// Spatial reflection probes share the IBL set (set 3, bindings 3-5) so the mesh layout stays
// within the maxBoundDescriptorSets budget. probeCubes[i] is probe i's prefiltered specular
// cube, probeIrradiance[i] its diffuse irradiance cube; probeMeta carries each probe's
// origin/radius/box/intensity. The fragment picks the nearest probe whose influence sphere
// contains the surface and blends its specular over the global IBL by an edge-soft weight.
// Iterated for the first ProbeMeta.count probes (globals.ambientColor.w); 0 = pure global
// fallback (byte-identical to no-probe rendering). Every array slot is seeded with the global
// IBL cubes, so unused slots harmlessly resolve to the global env.
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
// box (origin +- extent), returning a sample direction relative to the probe center. Used
// only when boxProjection is on; the infinite-distance cube uses R directly.
float3 boxProject(float3 worldPos, float3 R, float3 origin, float3 extent)
{
    float3 invR = 1.0 / R;
    float3 tMax = (origin + extent - worldPos) * invR;
    float3 tMin = (origin - extent - worldPos) * invR;
    float3 t = max(tMax, tMin);
    float d = min(min(t.x, t.y), t.z);
    return (worldPos + R * d) - origin;
}

// Set 6: the ray-tracing TLAS for inline ray-query shadows (pointShadowMeta.z gates it).
// Present only when the device supports RT (the PSO layout omits set 6 otherwise), but the
// binding compiles unconditionally — it is only accessed under the runtime flag.
[[vk::binding(0, 6)]] RaytracingAccelerationStructure rtScene;

// Trace one TerminateOnFirstHit shadow ray from worldPos toward `toLight` (a unit dir);
// returns 1 = lit (no occluder within maxDist), 0 = shadowed.
float rayQueryShadow(float3 worldPos, float3 toLight, float maxDist)
{
    RayDesc ray;
    ray.Origin = worldPos + toLight * 0.02;  // bias off the surface to avoid self-hit
    ray.Direction = toLight;
    ray.TMin = 0.0;
    ray.TMax = maxDist;
    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(rtScene, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xFF, ray);
    q.Proceed();
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT ? 0.0 : 1.0;
}

// Trace a reflection ray for the closest opaque hit; returns the hit distance (> 0) or -1 on
// a miss. Used by RT reflections (extraFlags.y) to find the reflected surface even off-screen.
float rayQueryReflection(float3 worldPos, float3 dir, float maxDist)
{
    RayDesc ray;
    ray.Origin = worldPos + dir * 0.02;  // bias off the surface to avoid self-hit
    ray.Direction = dir;
    ray.TMin = 0.0;
    ray.TMax = maxDist;
    RayQuery<RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(rtScene, RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xFF, ray);
    while (q.Proceed()) {}  // opaque triangle candidates auto-commit; loop finds the closest
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT ? q.CommittedRayT() : -1.0;
}

/// Builds the shared [`giprobe::DdgiVolume`] from this frame's light globals (the mesh's view of the
/// camera-centered probe clipmap) for `giprobe::ddgiSampleIrradiance`.
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

// Displacement lives entirely in the `displace` compute pre-pass now: it bakes each displacement
// material's height offset into the shared deformed vertex buffer, which every geometry pass (this
// vertex shader included) reads as an ordinary static stream — so a displaced instance is already
// displaced by the time it reaches here, consistently across the main view, depth, and every shadow
// map, and the buffer can back an RT BLAS. The vertex shader therefore does no displacement itself.
public VertexOutput transformVertex(VertexInput input, uint instanceIndex)
{
    Instance inst = instances[instanceIndex];
    VertexOutput output;
    float4 worldPos = mul(inst.model, float4(input.position, 1.0));
    output.worldPos = worldPos.xyz;
    output.position = mul(camera.viewProj, worldPos);
    output.worldNormal = mul((float3x3)inst.normalMatrix, input.normal);
    output.uv0 = input.uv0;
    output.materialIndex = inst.texture.w;
    return output;
}

// GPU skinning: blend the joint palette (worldBone * inverseBind, world-space) per
// vertex, then apply inst.model — identity for skinned draws, since per glTF the
// joints place the vertices entirely and the node transform is ignored.
public VertexOutput transformVertexSkinned(SkinnedVertexInput input, uint instanceIndex)
{
    Instance inst = instances[instanceIndex];
    uint base = inst.texture.y;  // this instance's offset into the frame's joint palette
    float4x4 skinMatrix = input.weights.x * jointMatrices[base + input.joints.x]
                        + input.weights.y * jointMatrices[base + input.joints.y]
                        + input.weights.z * jointMatrices[base + input.joints.z]
                        + input.weights.w * jointMatrices[base + input.joints.w];
    float4 skinnedPos = mul(skinMatrix, float4(input.position, 1.0));
    float3 skinnedNormal = mul((float3x3)skinMatrix, input.normal);

    VertexOutput output;
    float4 worldPos = mul(inst.model, skinnedPos);
    output.worldPos = worldPos.xyz;
    output.position = mul(camera.viewProj, worldPos);
    output.worldNormal = normalize(mul((float3x3)inst.normalMatrix, skinnedNormal));
    output.uv0 = input.uv0;
    output.materialIndex = inst.texture.w;
    return output;
}

// UE4-style punctual attenuation: inverse-square with a smooth range window.
float distanceAttenuation(float dist, float range)
{
    float invSquare = 1.0 / max(dist * dist, 0.0001);
    float t = saturate(1.0 - pow(dist / range, 4.0));
    return invSquare * t * t;
}

// Cook-Torrance metallic/roughness BRDF (energy-conserving). `radiance` is the
// incoming light's color * intensity * attenuation * cone; the cosine + diffuse/PI
// are applied here. Shared by the directional and the punctual lights.
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
    // Multi-scatter energy compensation (matches the IBL specular path): scale single-scatter GGX
    // back toward unity using the single-scatter directional albedo Ess = ab.x + ab.y (≈1 at low
    // roughness, so no boost on smooth metals; < 1 at high roughness).
    float2 lutAb = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
    spec *= 1.0 + F0 * (1.0 / max(lutAb.x + lutAb.y, 1e-3) - 1.0);
    float3 kd = (1.0 - F) * (1.0 - metallic);
    float3 diff = kd * albedo / PI;
    return (diff + spec) * radiance * ndotl;
}

// One punctual light's Cook-Torrance contribution at a world-space surface point.
// `lightIndex` lets the shadowed spot light apply its shadow-map visibility.
float3 punctual(GpuLight lt, uint lightIndex, float3 worldPos, float3 n, float3 v,
                float3 albedo, float metallic, float roughness)
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
    // Ray-traced punctual shadow (one ray toward the light) when RT shadows are on — applies
    // to every punctual light, vs the map paths that shadow only one spot + one point light.
    if (globals.pointShadowMeta.z != 0)
    {
        shadow = rayQueryShadow(worldPos, l, dist);
    }
    if (lt.directionType.w > 0.5)
    {
        float3 spotDir = normalize(lt.directionType.xyz);
        float cosAngle = dot(spotDir, -l);
        cone = smoothstep(lt.spotCos.y, lt.spotCos.x, cosAngle);
        if (globals.pointShadowMeta.z == 0 && globals.spotShadow.y != 0 && lightIndex == globals.spotShadow.x)
        {
            shadow = pcfShadow(spotShadowMap, globals.spotShadowViewProj, worldPos);
        }
    }
    else if (globals.pointShadowMeta.z == 0 && globals.pointShadowMeta.y != 0 && lightIndex == globals.pointShadowMeta.x)
    {
        shadow = pointShadow(worldPos);
    }
    float3 radiance = lt.colorIntensity.rgb * lt.colorIntensity.a * attenuation * cone * shadow;
    return brdf(n, v, l, albedo, metallic, roughness, radiance);
}

// Froxel cluster index for a fragment, from its pixel position + view-space depth.
uint clusterIndexFor(float2 pixel, float viewZ)
{
    float near = clusterParams.zPlanes.x;
    float far = clusterParams.zPlanes.y;
    float depth = max(-viewZ, near);
    uint zSlice = uint(log(depth / near) / log(far / near) * float(clusterParams.gridSize.z));
    zSlice = min(zSlice, clusterParams.gridSize.z - 1);

    float2 tileSize = float2(clusterParams.screenSize.xy) / float2(clusterParams.gridSize.xy);
    uint tileX = min(uint(pixel.x / tileSize.x), clusterParams.gridSize.x - 1);
    uint tileY = min(uint(pixel.y / tileSize.y), clusterParams.gridSize.y - 1);
    return tileX + tileY * clusterParams.gridSize.x + zSlice * clusterParams.gridSize.x * clusterParams.gridSize.y;
}

// The material seam: evalSurface turns interpolated vertex data + bound textures into the
// surface inputs the lighting consumes. Its body is the fixed glTF metallic-roughness path,
// or node-graph codegen; the lighting below it is shared across material variants.
public struct SurfaceData
{
    public float3 albedo;
    public float metallic;
    public float roughness;
    public float3 normal;     // world-space, normalized
    public float3 emissive;
    public float occlusion;   // 1.0 = none; multiplies the ambient term
    public float opacity;
};

public struct MaterialInput
{
    public float3 worldPos;
    public float3 worldNormal;
    public float3 viewDir;   // world-space, toward the eye (for parallax)
    public float2 uv0;
    public MaterialParams mat;
};

// Material feature bits (MaterialParams.tex1.w). Optional samples are taken only when set, so a
// material without the map pays nothing (a uniform, wave-coherent branch).
public static const uint FEATURE_NORMAL = 1u;
public static const uint FEATURE_EMISSIVE_TEX = 2u;
public static const uint FEATURE_OCCLUSION = 4u;
public static const uint FEATURE_HEIGHT = 8u;
public static const uint FEATURE_ALPHACLIP = 16u;
// The height map drives real geometry displacement (a `displace` compute pre-pass bakes it into the
// deformed vertex buffer — true silhouette, consistent across every pass, BLAS-able) rather than the
// fragment parallax march. The fragment still reads it for a height-gradient shading normal. Mutually
// exclusive with FEATURE_HEIGHT (the material sets one or neither).
public static const uint FEATURE_DISPLACE = 32u;

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

// Perturb the geometric normal N by a tangent-space normal-map sample (xyz in [-1, 1]).
public float3 perturbNormal(float3 N, float3 worldPos, float2 uv, float3 mapN)
{
    float3x3 tbn = cotangentFrame(N, worldPos, uv);
    return normalize(mul(mapN, tbn));
}

// Parallax occlusion mapping: march the tangent-space view ray through the height field and
// return the offset UV. Height map: white = high; vt = tangent-space view dir (toward eye).
public float2 parallaxUv(float2 uv, float3 vt, uint heightIndex, float scale)
{
    const int steps = 24;
    float layerDepth = 1.0 / float(steps);
    float2 delta = (vt.xy / max(abs(vt.z), 0.1)) * scale / float(steps);
    float curDepth = 0.0;
    float2 cur = uv;
    float h = 1.0 - albedoTextures[NonUniformResourceIndex(heightIndex)].SampleLevel(cur, 0.0).r;
    for (int i = 0; i < steps; i = i + 1)
    {
        if (curDepth >= h)
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
    float w = afterH / (afterH - beforeH);
    return lerp(cur, prev, w);
}


// Builds the material seam input from interpolated vertex data + the per-material params.
public MaterialInput makeMaterialInput(VertexOutput input)
{
    MaterialParams mp = materialParams[input.materialIndex];
    MaterialInput mi;
    mi.worldPos = input.worldPos;
    mi.worldNormal = input.worldNormal;
    mi.viewDir = normalize(globals.eyePosition.xyz - input.worldPos);
    mi.uv0 = input.uv0;
    mi.mat = mp;
    return mi;
}

// The debug render-output channel (pointShadowMeta.w); 0 = full shading. See `evalViewMode` for
// the channel → view-mode mapping (matches ViewMode::debug_channel in renderer.rs).
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

// The shared lighting half: turns a SurfaceData into lit radiance (direct + punctual + IBL/GI).
// Compiled once into this module; each material variant links it instead of recompiling it.
//
// `translucent` (the blend permutation) gates off every screen-space term — contact shadows, the
// screen-space GI resolve, GTAO, SSR, specular sky-occlusion, SSGI, and the ReSTIR direct sample —
// because a blend fragment's screen UV addresses the OPAQUE geometry behind it, not the fragment.
// It substitutes world-space indirect (analytic IBL irradiance + world-space DDGI), matching how
// forward renderers (UE forward-shaded translucency, Unity/URP transparents) light blended surfaces.
public float4 evalLighting(VertexOutput input, SurfaceData surf, bool translucent)
{
    float3 n = surf.normal;
    float3 albedo = surf.albedo;

    float metallic = surf.metallic;
    float roughness = surf.roughness;  // clamp away from 0 (specular singularity)
    float3 v = normalize(globals.eyePosition.xyz - input.worldPos);

    // Screen UV for sampling the screen-space effect maps.
    float2 screenUv = input.position.xy / float2(clusterParams.screenSize.xy);

    // Directional light, evaluated through the same BRDF as the punctual lights, then
    // attenuated by the shadow map + screen-space contact shadows (directional only).
    float3 lDir = -normalize(globals.directionAmbient.xyz);
    float shadow = 1.0;
    if (globals.pointShadowMeta.z != 0)
    {
        // Ray-traced directional shadow (a long ray toward the sun) — A/B vs the map.
        shadow = rayQueryShadow(input.worldPos, lDir, 1e4);
    }
    else if (globals.counts.y != 0)
    {
        shadow = pcfShadow(shadowMap, globals.shadowViewProj, input.worldPos);
    }
    if (globals.screenFlags.x != 0 && !translucent)
    {
        shadow *= contactMap.SampleLevel(screenUv, 0.0).r;  // fine contact detail (opaque only)
    }
    float3 lo = brdf(n, v, lDir, albedo, metallic, roughness,
                     globals.colorIntensity.rgb * globals.colorIntensity.a) * shadow;

    // Punctual lights. ReSTIR (screenFlags.w) replaces the per-light loop with one resolved
    // stochastic many-light direct radiance sample (diffuse term); otherwise loop the
    // froxel cluster's lights (clustered) or every light (reference).
    if (globals.screenFlags.w != 0 && !translucent)
    {
        // ReSTIR's resolved direct radiance is a screen-space buffer keyed to the opaque G-buffer, so
        // a translucent fragment falls through to the real clustered/reference punctual loop instead.
        float3 restir = restirRadiance.SampleLevel(screenUv, 0.0).rgb;
        lo += restir * albedo / PI;  // resolved radiance already includes geometry * vis * W
    }
    else if (clusterParams.screenSize.z != 0)
    {
        float viewZ = mul(clusterParams.view, float4(input.worldPos, 1.0)).z;
        uint clusterIndex = clusterIndexFor(input.position.xy, viewZ);
        uint count = clusters[clusterIndex].count;
        for (uint i = 0; i < count; i = i + 1)
        {
            uint li = clusters[clusterIndex].indices[i];
            lo += punctual(lights[li], li, input.worldPos, n, v, albedo, metallic, roughness);
        }
    }
    else
    {
        for (uint i = 0; i < globals.counts.x; i = i + 1)
        {
            lo += punctual(lights[i], i, input.worldPos, n, v, albedo, metallic, roughness);
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
        // Analytic IBL sky irradiance along the shading normal. It is the residual indirect
        // diffuse where the DDGI cage does not cover the surface; the DDGI replace below overrides
        // it wherever coverage exists (DDGI carries the sky occlusion intrinsically, via ray-miss).
        float3 irradiance = irradianceMap.SampleLevel(n, 0.0).rgb;
        float3 R = reflect(-v, n);
        float3 prefiltered = prefilteredMap.SampleLevel(R, prefilterLod(roughness)).rgb;

        // Reflection probes: find the nearest covering probe and blend its prefiltered specular +
        // irradiance over the global IBL by an edge-soft weight (1 at center, 0 at the boundary).
        // ambientColor.w bit-packs the probe count (CPU bit_casts the u32, so read it back the
        // same way); 0 leaves both terms exactly at the global value.
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

        // Screen-space reflections replace the prefiltered-env radiance where they hit, so
        // a smooth metal mirrors the actual neighbouring geometry (the cube only holds the
        // environment). Weighted by hit confidence × a smoothness gate (full at roughness 0,
        // zero past ~0.4) so rough surfaces keep the cube fallback. Applied at the radiance
        // level so the BRDF + multi-scatter + horizon + specular-AO below treat it uniformly.
        if (globals.extraFlags.x != 0 && !translucent)
        {
            float4 ssr = ssrMap.SampleLevel(screenUv, 0.0);
            float gate = saturate(1.0 - roughness * 2.0);
            prefiltered = lerp(prefiltered, ssr.rgb, ssr.a * gate);
        }

        // Ray-traced reflections: trace the reflection ray against the TLAS (finds geometry
        // off-screen, where SSR fails) and reproject the world-space hit through last frame's
        // view-proj into prev_color for its reflected radiance. Gated to smooth surfaces; an
        // off-screen reprojection or a missed ray falls back to the (SSR-or-env) radiance.
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
                            float3 rtRefl = prevColorMesh.SampleLevel(huv, 0.0).rgb;
                            prefiltered = lerp(prefiltered, rtRefl, gate);
                        }
                    }
                }
            }
        }

        // Specular sky occlusion: a cone along the reflection vector R (widened by roughness)
        // sphere-marched against the Global Distance Field — the reflected skybox is occluded only
        // where the reflection direction is blocked, NOT by the diffuse AO scalar (which would
        // wrongly dim a chrome surface facing open sky). The march runs in the reduced-resolution
        // specocc prepass (denoised + temporally accumulated); the mesh samples the resolved map.
        // Off (1.0) unless enabled this frame.
        float specSkyVis = (globals.sdfOcclusion.y != 0 && !translucent) ? speoccMap.SampleLevel(screenUv, 0.0).r : 1.0;

        float2 ab = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
        float3 specularIBL = prefiltered * (F0 * ab.x + ab.y);
        // Multi-scatter energy compensation (Fdez-Aguera / Filament 4.7.2): single-scatter GGX
        // loses energy as roughness rises (a rough metal darkens up to ~60% at roughness 1). `Ess`
        // is the single-scatter directional albedo (the white-F0 reflectance = ab.x + ab.y), which is
        // ~1 at low roughness (→ no boost on smooth mirrors) and < 1 at high roughness (→ scaled back
        // toward unity). Using ab.y alone would explode as ab.y→0 on smooth metals — the bug that
        // blew them out.
        float Ess = ab.x + ab.y;
        float3 energyComp = 1.0 + F0 * (1.0 / max(Ess, 1e-3) - 1.0);
        specularIBL *= energyComp;
        // Horizon occlusion: a reflection ray pointing below the geometric surface can't see the
        // environment, so fade it out — otherwise a curved/normal-mapped metal leaks reflections
        // from under itself at grazing angles. Uses the GEOMETRIC vertex normal, not the shading n.
        float horizon = min(1.0 + dot(R, normalize(input.worldNormal)), 1.0);
        specularIBL *= horizon * horizon;
        // Occlusion band split: large-range occlusion of indirect DIFFUSE is the DDGI ray-miss
        // replace below (the sky reaches the surface only on a ray that escapes), so the diffuse
        // term here carries only the material occlusion texture + contact-scale GTAO. GTAO is the
        // contact term: this `ao` (material × GTAO, small radius) drives the roughness-aware
        // specular-AO (Lagarde, → 1 on smooth metals so a chrome sphere keeps its reflection in
        // creases), the contact darkening of the indirect diffuse, and the SSGI bounce below.
        float ao = surf.occlusion;  // material occlusion (1.0 = none)
        if (globals.counts.w != 0 && !translucent)
        {
            ao *= aoMap.SampleLevel(screenUv, 0.0).r;  // screen-space GTAO (contact scale, opaque only)
        }
        float specAO = saturate(pow(ndotv + ao, exp2(-16.0 * roughness - 1.0)) - 1.0 + ao);
        specularIBL *= specAO * specSkyVis;  // reflection-cone occludes the reflected skybox
        reflectionSpec = specularIBL;

        // Indirect diffuse irradiance (the DDGI cage replacing the analytic IBL diffuse by coverage,
        // both occluded by the DFAO sky-visibility) is resolved once per HALF-res pixel in the
        // gi-resolve pass (view-independent), not per full-res fragment — the fragment samples the
        // bilinearly-upsampled result here and applies the per-pixel kd·albedo tail. This is why the
        // 8-probe DDGI cage + IBL diffuse cube are no longer sampled in this fragment. See
        // `gi_resolve.slang`.
        float3 indirectIrr;
        if (translucent)
        {
            // Translucent surfaces can't read the screen-space GI resolve (it's keyed to the opaque
            // G-buffer behind them), so take indirect diffuse from world-space sources: the analytic
            // IBL sky irradiance (already reflection-probe-blended above), overlaid by the world-space
            // DDGI cage where it has coverage — the same terms the fallback path uses.
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
            indirectIrr = giIndirectMap.SampleLevel(screenUv, 0.0).rgb;
        }
        float3 indirect = kd * indirectIrr * albedo;
        // Indirect diffuse occlusion: the large-range term is the DDGI ray-miss already folded into
        // indirectIrr (the sky enters only on a ray that escapes), so the only further occlusion is
        // contact-scale — the material texture × small-radius GTAO (`ao`) — filling in the contact
        // detail the coarse probe grid cannot resolve, with no large-range double-count.
        ambient = indirect * ao + specularIBL;
        // SSGI adds one-bounce screen-space indirect on top (it is a bounce, not the sky term),
        // modulated by AO so occluded creases don't over-bounce. Screen-space, so opaque only.
        if (globals.screenFlags.y != 0 && !translucent)
        {
            float3 gi = ssgiMap.SampleLevel(screenUv, 0.0).rgb;
            ambient += gi * albedo * (1.0 - metallic) * ao;
        }
    }
    else
    {
        float ao = (globals.counts.w != 0 && !translucent) ? aoMap.SampleLevel(screenUv, 0.0).r : 1.0;
        ambient = albedo * (1.0 - metallic) * globals.ambientColor.rgb * surf.occlusion * ao;
        // SSGI bounce on the non-IBL fallback path (additive, AO-modulated). Screen-space, opaque only.
        if (globals.screenFlags.y != 0 && !translucent)
        {
            float3 gi = ssgiMap.SampleLevel(screenUv, 0.0).rgb;
            ambient += gi * albedo * (1.0 - metallic) * ao;
        }
        // DDGI on the fallback path: no analytic IBL diffuse to replace, so add the cage
        // irradiance directly (weighted by coverage to avoid a hard edge at the cage boundary).
        if (globals.screenFlags.z != 0)
        {
            float4 ddgi = ddgiSampleIrradiance(
                ddgiVolumeFromGlobals(), ddgiIrradiance, ddgiDistance, input.worldPos, n);
            ambient += ddgi.rgb * ddgi.w * albedo * (1.0 - metallic);
        }
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

// Resolves the active view mode (debugViewChannel) to a final color. Channel 0 is full shading;
// `kUnlit` (the per-material unlit PSO permutation) also forces the unlit output. Pure surface and
// screen-space channels return here; material-override modes recolour the surface and fall through
// to evalLighting (which itself peels off the Reflections/GI terms by channel). `Wireframe`,
// `LitWireframe`, and `MotionVectors` are channel 0 here — produced by dedicated passes.
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
        float ao = globals.counts.w != 0 ? aoMap.SampleLevel(screenUv, 0.0).r : 1.0;
        return float4(ao.xxx, 1.0);
    }

    // Punctual-light complexity as a heatmap: the number of point/spot lights reaching this
    // pixel (directional lights are not clustered, so they never count). 0 reads as near-black;
    // the ramp saturates to red around 8 overlapping lights.
    if (channel == 13u)
    {
        uint count;
        if (clusterParams.screenSize.z != 0)
        {
            // Clustered: the froxel's assigned light count (lights whose sphere hits the froxel).
            float viewZ = mul(clusterParams.view, float4(input.worldPos, 1.0)).z;
            count = clusters[clusterIndexFor(input.position.xy, viewZ)].count;
        }
        else
        {
            // Brute force: count punctual lights whose range reaches this pixel, matching the
            // reference lighting loop (so the mode is meaningful with clustering off).
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

    return evalLighting(input, surf, translucent);
}
