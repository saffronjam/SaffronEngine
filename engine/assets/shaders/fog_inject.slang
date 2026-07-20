// Froxel volumetric fog — injection (Wronski 2014 / Hillaire 2015). One thread per froxel over the
// frustum-aligned `FROXEL_GRID_{X,Y,Z}` grid: reconstruct the froxel-center world position from the
// exponential-Z view distance + the inverse view-proj, evaluate the participating-medium extinction
// (the Phase-1 analytic height density as the base medium + a constant `baseDensity`), and accumulate
// in-scatter over the containing cull cluster's light list — the directional sun (cascade PCF), the
// clustered point/spot lights (map shadows), and an isotropic ambient term — each weighted by the
// Henyey-Greenstein phase. Writes `(L_scat.rgb, sigma_t)` into the persistent scatter volume.
//
// Set 0 is the mesh light set (reused verbatim: globals/lights/clusters/clusterParams + shadow maps,
// with COMPUTE added to its stage flags). Set 1 is the fog volume: the scatter storage image + the
// froxel-grid UBO. The binding-free light/shadow math is shared with the forward mesh path, with the
// HG phase substituting for the surface BRDF.

import lighting_common;

[[vk::binding(0, 0)]] ConstantBuffer<LightGlobals> globals;
[[vk::binding(1, 0)]] StructuredBuffer<GpuLight> lights;
[[vk::binding(2, 0)]] StructuredBuffer<Cluster> clusters;
[[vk::binding(3, 0)]] ConstantBuffer<ClusterParams> clusterParams;
[[vk::binding(4, 0)]] Sampler2DShadow shadowMap;
[[vk::binding(5, 0)]] Sampler2DShadow spotShadowMap;
[[vk::binding(6, 0)]] SamplerCube pointShadowMap;
[[vk::binding(7, 0)]] SamplerCube pointShadowMapDynamic;
[[vk::binding(12, 0)]] Sampler2DArray<float> cloudShadowMap;

// The froxel-grid params UBO (mirrors `froxel_fog::FogGridParams`, 288 B std140).
struct FogGrid
{
    float4x4 invProjection; // clip -> view (froxel-center reconstruction)
    float4x4 invView;       // view -> world
    float4x4 prevViewProj;  // last frame's un-jittered world -> clip (temporal reprojection)
    uint4 gridSize;         // xyz froxel dims, w punctual light count
    float4 screenSize;      // xy offscreen pixels
    float4 zPlanes;         // x near, y far (shared with the cull), z froxel-far coverage
    float4 temporal;        // x historyBlend, y historyValid, z neighborhoodClamp, w lightClamp
    float4 jitter;          // xy sub-froxel NDC jitter, z jitter phase index, w scene time (s)
    float4 globalWind;      // xyz shared wind velocity, w time-of-day clock (s)
};

// One local FogVolume (mirrors `froxel_fog::FogVolumeGpu`, std430, 160 B). `localFromWorld` frames the
// bounds test + noise; the optical fields are packed into vec4 slots.
struct FogVolumeGpu
{
    float4x4 localFromWorld;
    float4 extentsShape;        // xyz box half-extents, w shape tag
    float4 radiusEdgeDensityHF; // x radius, y edgeFalloff, z density, w heightFalloff
    float4 albedoPhase;         // xyz single-scatter albedo, w HG anisotropy
    float4 emissiveSpeed;       // xyz emissive, w advection speed
    float4 noiseParams;         // x scale, y intensity, z detail
    float4 wind;                // xyz advection direction
};

static const uint FOG_SHAPE_BOX = 0;
static const uint FOG_SHAPE_SPHERE = 1;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 1)]] RWTexture3D<float4> scatterExtinction; // rgb = L_scat, a = sigma_t (this frame)
[[vk::binding(1, 1)]] ConstantBuffer<FogGrid> grid;
[[vk::binding(2, 1)]] Sampler3D<float4> scatterHistory;      // last frame's linear scatter+extinction
[[vk::binding(3, 1)]] StructuredBuffer<FogVolumeGpu> fogVolumes; // bounded local-density list
[[vk::binding(4, 1)]] Sampler3D<float> noiseTex;                 // tiling erosion noise (R8, repeat)

// The authored fog medium (the analytic height density injected as the froxel base medium — never
// re-applied at composite — plus the scattering albedo, phase anisotropy, and constant emission).
struct InjectPush
{
    float3 eyeWorld;     // world-space camera position (the view vector origin)
    float baseDensity;   // constant sigma_t floor
    float3 emissive;     // constant in-medium emission
    float scatterAlbedo; // sigma_s = albedo * sigma_t
    float4 layer0;       // x density, y heightFalloff, z height, w phaseG (broad layer)
    float4 layer1;       // x density, y heightFalloff, z height (ground haze; density 0 disables)
};
[[vk::push_constant]] InjectPush push;

// The van der Corput radical inverse (base 2), remapped to [-0.5, 0.5] — the per-frame sub-slice
// jitter for the Z axis, walking the froxel interior over the jitter cycle so the coarse Z grid
// supersamples a crisp shaft edge through the temporal accumulation.
float haltonZ(uint index)
{
    uint bits = index;
    bits = (bits << 16) | (bits >> 16);
    bits = ((bits & 0x55555555u) << 1) | ((bits & 0xAAAAAAAAu) >> 1);
    bits = ((bits & 0x33333333u) << 2) | ((bits & 0xCCCCCCCCu) >> 2);
    bits = ((bits & 0x0F0F0F0Fu) << 4) | ((bits & 0xF0F0F0F0u) >> 4);
    bits = ((bits & 0x00FF00FFu) << 8) | ((bits & 0xFF00FF00u) >> 8);
    return float(bits) * 2.3283064365386963e-10 - 0.5;
}

// The jittered sub-slice fraction for froxel slice `fz`: the slice-center fraction nudged by the
// per-frame Halton offset so successive frames sample distinct depths inside the slice.
float froxelWFraction(uint fz)
{
    return (float(fz) + 0.5 + haltonZ(uint(grid.jitter.z))) / float(grid.gridSize.z);
}

// View-space Z (negative-forward) at fractional slice depth `wFrac`: the exponential-Z distance.
float froxelViewZAt(float wFrac)
{
    float near = grid.zPlanes.x;
    float far = grid.zPlanes.y;
    return -near * pow(far / near, wFrac);
}

// The jittered froxel-sample world position: the exponential-Z view distance at the jittered slice
// fraction, unprojected through the sub-froxel-jittered tile centre and moved to world space (the
// inverse of the composite's W mapping). The XY jitter reuses the TAA NDC offset (phase-locked).
float3 froxelSampleWorld(uint3 f, float viewZ)
{
    float2 uv = (float2(f.xy) + 0.5 + grid.jitter.xy * 0.5) / float2(grid.gridSize.xy);
    float2 ndc = uv * 2.0 - 1.0;
    float4 r = mul(grid.invProjection, float4(ndc, 1.0, 1.0));
    float3 viewRay = r.xyz / r.w;
    float3 viewPos = viewRay * (viewZ / viewRay.z);
    return mul(grid.invView, float4(viewPos, 1.0)).xyz;
}

// Reproject a world position into last frame's froxel `uvw`: prev clip -> NDC XY -> [0,1]^2, and the
// linear view distance (clip.w == -viewZ for the RH projection) mapped through the same exponential-Z
// curve the forward march inverts. Returns uvw; the caller gates on the in-bounds range.
float3 froxelUvwFromWorld(float3 worldPos)
{
    float4 clip = mul(grid.prevViewProj, float4(worldPos, 1.0));
    float3 uvw;
    uvw.xy = (clip.xy / clip.w) * 0.5 + 0.5;
    float near = grid.zPlanes.x;
    float far = grid.zPlanes.y;
    float depth = max(clip.w, near);
    uvw.z = log(depth / near) / log(far / near);
    return uvw;
}

// Exponential-in-height density at a world point (the Phase-1 analytic layer evaluated as a point
// sigma, not integrated — the front-to-back march integrates it along Z).
float heightDensity(float4 layer, float worldY)
{
    if (layer.x <= 0.0)
    {
        return 0.0;
    }
    return layer.x * exp2(-layer.y * (worldY - layer.z));
}

// Signed distance to an axis-aligned box of half-extents `b` (in the box's local frame): negative
// inside, zero on the surface, positive outside.
float sdBox(float3 p, float3 b)
{
    float3 q = abs(p) - b;
    return length(max(q, 0.0)) + min(max(q.x, max(q.y, q.z)), 0.0);
}

// The erosion multiplier for a local volume at `worldPos`: a low-frequency tiling base octave minus a
// higher-frequency detail octave (the Frostbite/Horizon subtractive-erosion shape), drifted on
// `wind * speed * time`, blended toward 1 by `noiseIntensity` (0 → uniform, the fetch is skipped).
float fogVolumeNoise(float3 worldPos, FogVolumeGpu v)
{
    float intensity = v.noiseParams.y;
    if (intensity <= 0.0)
    {
        return 1.0;
    }
    float scale = v.noiseParams.x;
    float detail = v.noiseParams.z;
    bool hasLocalWind = dot(v.wind.xyz, v.wind.xyz) > 1e-8;
    float3 velocity = hasLocalWind ? v.wind.xyz * v.emissiveSpeed.w : grid.globalWind.xyz;
    float time = hasLocalWind ? grid.jitter.w : grid.globalWind.w;
    float3 drift = velocity * time;
    float3 p = (worldPos - drift) * scale;
    float base = noiseTex.SampleLevel(p, 0.0);
    float det = noiseTex.SampleLevel(p * 2.02 + 11.5, 0.0);
    float erosion = saturate(base - detail * det);
    return lerp(1.0, erosion, intensity);
}

[shader("compute")]
[numthreads(8, 8, 4)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    if (any(tid >= grid.gridSize.xyz))
    {
        return;
    }

    float wFrac = froxelWFraction(tid.z);
    float viewZ = froxelViewZAt(wFrac);
    float3 worldPos = froxelSampleWorld(tid, viewZ);
    float3 viewDir = normalize(push.eyeWorld - worldPos);

    float sigmaT = max(push.baseDensity
                       + heightDensity(push.layer0, worldPos.y)
                       + heightDensity(push.layer1, worldPos.y), 0.0);
    // sigma_s carries colour so a local volume's tinted albedo folds into the shared scatter term;
    // the global base uses its grey scalar albedo.
    float3 sigmaS = float3(push.scatterAlbedo, push.scatterAlbedo, push.scatterAlbedo) * sigmaT;
    float3 emissive = push.emissive;
    // The froxel's phase g is the sigma_t-weighted mean of every contributing medium, so a single HG
    // evaluation in the light loop stays correct where volumes with different anisotropy overlap.
    float phaseWeighted = push.layer0.w * sigmaT;
    float phaseWeightSum = sigmaT;

    // Local FogVolumes: each accumulates its soft-edged, height-slabbed, noise-eroded density into the
    // same medium the analytic base writes — no bespoke local-fog pass, one shared integrate.
    uint volumeCount = asuint(push.layer1.w);
    for (uint vi = 0; vi < volumeCount; vi = vi + 1)
    {
        FogVolumeGpu v = fogVolumes[vi];
        float3 local = mul(v.localFromWorld, float4(worldPos, 1.0)).xyz;
        uint shape = uint(v.extentsShape.w + 0.5);
        float d = (shape == FOG_SHAPE_BOX)
                      ? sdBox(local, v.extentsShape.xyz)
                      : (length(local) - v.radiusEdgeDensityHF.x);
        float edgeFalloff = max(v.radiusEdgeDensityHF.y, 1e-4);
        if (d >= edgeFalloff)
        {
            continue; // outside the soft margin
        }
        float edge = smoothstep(edgeFalloff, 0.0, d); // 1 interior → 0 at the boundary
        float heightFalloff = v.radiusEdgeDensityHF.w;
        float slab = (heightFalloff > 0.0) ? exp2(-heightFalloff * max(local.y, 0.0)) : 1.0;
        float sigma = max(v.radiusEdgeDensityHF.z, 0.0) * edge * slab * fogVolumeNoise(worldPos, v);
        if (sigma <= 0.0)
        {
            continue;
        }
        sigmaT += sigma;
        sigmaS += v.albedoPhase.xyz * sigma;
        emissive += v.emissiveSpeed.xyz * sigma;
        phaseWeighted += v.albedoPhase.w * sigma;
        phaseWeightSum += sigma;
    }
    float phaseG = (phaseWeightSum > 1e-6) ? (phaseWeighted / phaseWeightSum) : push.layer0.w;

    // The containing cull cluster's light list (the coarse 16x9x24 grid), reconstructed from the
    // froxel-center pixel + view-space Z exactly as the mesh path reconstructs its cluster.
    float2 pixel = (float2(tid.xy) + 0.5) * grid.screenSize.xy / float2(grid.gridSize.xy);
    uint cluster = clusterIndexFor(clusterParams, pixel, viewZ);

    float lightClamp = grid.temporal.w;
    float3 inScatter = fogDirectionalInScatter(worldPos, viewDir, phaseG, globals, shadowMap);
    inScatter *= cloudShadowVisibility(
        worldPos, cloudShadowMap, globals, globals.cloudShadowMeta.y);
    if (lightClamp > 0.0)
    {
        inScatter = min(inScatter, lightClamp);
    }
    inScatter += fogAmbientInScatter(globals);

    uint count = clusters[cluster].count;
    for (uint i = 0; i < count; i = i + 1)
    {
        uint li = clusters[cluster].indices[i];
        float3 c = fogPunctualInScatter(lights[li], li, worldPos, viewDir, phaseG,
                                        globals, spotShadowMap, pointShadowMap, pointShadowMapDynamic);
        // Per-light firefly clamp (off when lightClamp == 0): cap a single light's in-scatter before
        // accumulation so a bright light grazing a shadow edge cannot inject a single-froxel spike.
        inScatter += (lightClamp > 0.0) ? min(c, lightClamp) : c;
    }

    float4 fresh = float4(inScatter * sigmaS + emissive, sigmaT);

    // Temporal reprojection (Hillaire 2015): blend the LINEAR (L_scat, sigma_t) — never the
    // integrated transmittance — with last frame's reprojected sample. The front-to-back integrate
    // (unchanged) still marches this fresh each frame off the reprojected volume.
    float3 hUvw = froxelUvwFromWorld(worldPos);
    bool reuse = grid.temporal.y != 0.0 && all(hUvw >= 0.0) && all(hUvw <= 1.0);
    float4 outv = fresh;
    if (reuse)
    {
        float4 hist = scatterHistory.SampleLevel(hUvw, 0.0);
        // Optional neighbourhood clamp: keep the reprojected history within a band of the fresh
        // sample so a fast-moving light cannot ghost. Off by default (temporal.z == 0).
        if (grid.temporal.z != 0.0)
        {
            hist = clamp(hist, fresh * 0.5 - 1e-4, fresh * 2.0 + 1e-4);
        }
        outv = lerp(hist, fresh, grid.temporal.x);
    }
    scatterExtinction[tid] = outv;
}
