module sdf;

// Re-export the octahedral encode so `import sdf` consumers (the DDGI ray trace) keep getting
// octEncode/octDecode from here; the encode itself lives in the resource-free `octahedral`
// module so consumers can share it without importing this module's bindless field bindings.
__exported import octahedral;

// Re-export the per-mesh brick sample (`sampleMdfBrick`, `SdfInstance`, `SdfSample` + the set-0
// bindless brick arrays). One brick-sampling implementation backs both this module's near-field
// sample and the Global-SDF composite (`gdf_composite.slang`).
__exported import mdf_brick;

// Shared signed-distance-field sampling. The specular reflection-occlusion prepass
// (`specocc.slang`) links this module for the GDF reflection-occlusion cone
// (`sdfReflectionOcclusion`); the DFAO prepass for the hemisphere sky-visibility cone
// (`sdfSkyVisibility`); the DDGI ray trace for the unified near/far field sphere-march
// (`sampleField`). The resource bindings travel with the linked functions: set 0 (the bindless
// brick arrays, from `mdf_brick`), set 1 binding 8 the per-mesh SDF-occluder instance list (the
// near field, used only by the DDGI trace), and set 1 bindings 9/10 the Global-SDF cascade
// clipmap + its params — every consuming pipeline's layout provides exactly the bindings its
// linked functions reach (the reflection/sky occlusion reach only the GDF tap).

// The number of Global-SDF cascades (matches `global_sdf.rs::GDF_CASCADES`).
static const uint kGdfCascades = 3u;

// Set 1, binding 8: the per-mesh SDF-occluder instance list, cone-traced for the near field.
[[vk::binding(8, 1)]] StructuredBuffer<SdfInstance> sdfInstances;

// Set 1, binding 9: the Global-SDF cascade clipmap — `kGdfCascades` camera-centered `R16_SNORM`
// distance volumes, finest first. Sampled with a linear **repeat** sampler so the toroidal
// (wrap-modulo) addressing reads spatially-adjacent texels across the storage wrap (a recenter
// scrolls in only the leading slab, the rest of the volume keeps its physical location).
[[vk::binding(9, 1)]] Sampler3D gdfCascades[kGdfCascades];

// One cascade's world placement + encode scale (matches `global_sdf.rs::GdfCascadeUbo`).
public struct GdfCascade
{
    float4 centerHalf;   // xyz = snapped world center, w = half-extent (world)
    float4 voxelEncode;  // x = voxel size, y = max-encode dist (world), z = world extent (= voxel*res), w reserved
};

// Set 1, binding 10: the Global-SDF params (matches `global_sdf.rs::GdfParamsUbo`).
public struct GdfParams
{
    GdfCascade cascades[kGdfCascades];
    float4 control;  // x = enabled (0/1), y = active cascade count, z = near-handoff radius (world), w = overlap-band fraction
};
[[vk::binding(10, 1)]] ConstantBuffer<GdfParams> gdfParams;

// Probes every per-mesh SDF occluder at a world sample given the cone-footprint radius
// (`footprint`, world units). The global minimum signed distance over instances is kept;
// `openness` is the min over instances so the cone can early-out only where every nearby block
// is open. Each instance's tap is `sampleMdfBrick` (the one brick-sampling implementation).
public SdfSample sdfSample(float3 wp, uint count, float footprint)
{
    SdfSample s;
    s.distance = 1e30;
    s.openness = 1.0;
    for (uint i = 0; i < count; i = i + 1)
    {
        SdfSample inst = sampleMdfBrick(sdfInstances[i], wp, footprint);
        s.distance = min(s.distance, inst.distance);
        s.openness = min(s.openness, inst.openness);
    }
    return s;
}

// One trilinear tap of the Global Distance Field at a world sample: selects the finest cascade
// whose bounds contain `wp` (minus a wrap-seam margin), converts world → that cascade's toroidal
// `[0,1]³` UVW (`wp / worldExtent`, the repeat sampler wraps), and denormalizes the `R16_SNORM`
// by the cascade's max-encode distance. Near the cascade's outer edge it blends into the next
// coarser cascade over an overlap band so the cascade boundary shows no shell. `valid` is false
// (and the return undefined) when `wp` lies beyond the coarsest cascade — the caller then falls
// back to the per-mesh near field / treats it as open sky.
public float gdfDistance(float3 wp, out bool valid)
{
    valid = false;
    uint count = min(uint(gdfParams.control.y), kGdfCascades);
    float bandFrac = gdfParams.control.w;
    for (uint c = 0; c < count; c = c + 1)
    {
        GdfCascade cas = gdfParams.cascades[c];
        float half = cas.centerHalf.w;
        float voxel = cas.voxelEncode.x;
        float margin = voxel * 2.0;  // stay clear of the toroidal wrap seam at the outer faces
        float3 cmin = cas.centerHalf.xyz - half;
        float3 cmax = cas.centerHalf.xyz + half;
        if (any(wp <= cmin + margin) || any(wp >= cmax - margin)) { continue; }

        float worldExtent = cas.voxelEncode.z;
        float3 uvw = wp / worldExtent;  // toroidal: the repeat sampler wraps modulo the extent
        float d = gdfCascades[c].SampleLevel(uvw, 0.0).r * cas.voxelEncode.y;

        // Overlap band: near this cascade's outer face, blend with the next coarser cascade so
        // the handoff is seamless (and the seam-margin band is covered entirely by the coarser).
        float3 toFace = min(wp - cmin, cmax - wp);
        float edge = min(min(toFace.x, toFace.y), toFace.z);
        float bandWidth = max(half * bandFrac, voxel);
        if (c + 1u < count && edge < bandWidth)
        {
            GdfCascade nx = gdfParams.cascades[c + 1u];
            float dn = gdfCascades[c + 1u].SampleLevel(wp / nx.voxelEncode.z, 0.0).r * nx.voxelEncode.y;
            float t = saturate((edge - margin) / max(bandWidth - margin, 1e-4));  // 1 inner, 0 at the seam
            d = lerp(dn, d, t);
        }
        valid = true;
        return d;
    }
    return 1e30;
}

// Whether the Global Distance Field is enabled this frame (the DDGI trace gates its albedo-cache
// read on it; the cache is only populated when the GDF composite ran).
public bool gdfEnabled()
{
    return uint(gdfParams.control.x) != 0u;
}

// The finest cascade's full world extent (= voxel · resolution). The DDGI trace's lite albedo
// cache is aligned to this cascade, so a hit point maps to its toroidal UVW as `wp / extent`.
public float gdfFinestWorldExtent()
{
    return gdfParams.cascades[0].voxelEncode.z;
}

// The unified near/far field sample for the cone march: the per-mesh MDF for the near field
// (full resolution where leaks matter — the README's leak mitigation) and a single Global-SDF
// tap beyond the handoff radius, so far-field cost decouples from instance count. Falls back to
// the per-mesh field when the GDF is off or the sample lies beyond the coarsest cascade. Public so
// the DDGI ray trace (`ddgi_trace.slang`) sphere-marches the same near/far handoff the per-pixel
// cone trace does.
public SdfSample sampleField(float3 wp, uint count, float footprint, float t)
{
    bool useGdf = (uint(gdfParams.control.x) != 0u) && (t >= gdfParams.control.z);
    if (useGdf)
    {
        bool valid;
        float d = gdfDistance(wp, valid);
        if (valid)
        {
            SdfSample s;
            s.distance = d;
            // Openness for the early-out + the leap: open ahead when the field distance exceeds
            // the cone footprint here, blocked as it approaches a surface.
            s.openness = saturate(d / max(footprint, 1e-4));
            return s;
        }
    }
    return sdfSample(wp, count, footprint);
}

// Specular sky occlusion on the Global Distance Field: sphere-march one cone along the reflection
// vector `R`, its half-angle widening with roughness (a mirror sees a thin pencil of the sky; a
// rough surface a broad cone). Returns a [0,1] factor occluding the reflected skybox where `R` is
// blocked, independent of the diffuse AO so a chrome surface facing open sky keeps its reflection.
// Run by the reduced-resolution specocc prepass (denoised + temporally accumulated), it taps the
// GDF clipmap (`gdfDistance`), not the per-mesh near field. Returns open (1.0) when the GDF is off
// or the march escapes the cascade coverage (the reflection then reaches open sky as the analytic
// env intends).
public float sdfReflectionOcclusion(float3 worldPos, float3 R, float roughness)
{
    if (!gdfEnabled()) { return 1.0; }
    // Cone footprint per unit length: tan(half-angle), from a thin 0.05 at mirror-smooth to a
    // wide ~0.6 at full roughness.
    float coneHalfAngle = lerp(0.05, 0.6, roughness);
    const float kStart = 0.1;
    const float kStepFloor = 0.05;
    const float kMaxDist = 12.0;
    const int kSteps = 12;
    float occ = 1.0;
    float t = kStart;
    for (int s = 0; s < kSteps; s = s + 1)
    {
        float footprint = coneHalfAngle * t;
        bool valid;
        float d = gdfDistance(worldPos + R * t, valid);
        // The reflection left the cascade coverage: nothing more to occlude with, so stop and keep
        // the open fraction accumulated so far.
        if (!valid) { break; }
        // Penumbra: the nearest surface subtends `d / (coneHalfAngle * t)` of the cone; the
        // smallest such ratio along the cone is its open fraction.
        occ = min(occ, saturate(d / max(footprint, 1e-4)));
        // Already fully open and far from any surface — the rest of the march can only stay open.
        if (occ > 0.99 && d > footprint * 4.0) { break; }
        t += clamp(d, kStepFloor, kMaxDist);
        if (t > kMaxDist) { break; }
    }
    return occ;
}

/// Diffuse sky-visibility on the Global Distance Field (DFAO — Wright, "Dynamic Occlusion
/// with Signed Distance Fields", SIGGRAPH 2015): cone-trace the hemisphere of `N` against the
/// composited GDF clipmap and return a [0,1] factor occluding the analytic sky irradiance
/// where nearby geometry blocks the sky. Nine cones is the fewest that still separates an
/// enclosed interior from open sky. Each cone is sphere-marched for its penumbra (the min of
/// `d / coneRadius` along it, clamped to >= 0 so interior negative distances never brighten),
/// and the cones are cosine-weighted by `dot(coneDir, N)`. Returns open (1.0) when the GDF is
/// off or a cone escapes the cascade coverage (that cone then reaches open sky as the analytic
/// env intends). This taps the GDF clipmap, not the per-mesh near field.
///
/// `azimuthJitter` rotates the cone ring around `N` (radians): the reduced-resolution DFAO
/// prepass advances it per frame so the temporal accumulator supersamples the penumbra across
/// frames rather than re-tracing the same nine directions. Pass 0 for an unjittered trace.
public float sdfSkyVisibility(float3 worldPos, float3 N, float azimuthJitter)
{
    if (!gdfEnabled()) { return 1.0; }

    // Branchless orthonormal tangent frame around N (Duff et al. 2017).
    float sgn = N.z >= 0.0 ? 1.0 : -1.0;
    float a = -1.0 / (sgn + N.z);
    float b = N.x * N.y * a;
    float3 T = float3(1.0 + sgn * N.x * N.x * a, sgn * b, -sgn * N.x);
    float3 B = float3(b, sgn + N.y * N.y * a, -N.y);

    // Rotate the tangent frame around N by the per-frame jitter so successive frames trace a
    // rotated cone ring; the temporal accumulator averages them into a denser hemisphere.
    float cj = cos(azimuthJitter);
    float sj = sin(azimuthJitter);
    float3 Tj = cj * T + sj * B;
    float3 Bj = -sj * T + cj * B;
    T = Tj;
    B = Bj;

    // Nine hemisphere cone axes in tangent space (z = N): one along N plus two rings of four
    // ( inner ~35 deg, outer ~65 deg from N ) — the DFAO 9-cone hemisphere set.
    const float3 kCones[9] = {
        float3(0.0, 0.0, 1.0),
        float3(0.574, 0.0, 0.819),
        float3(0.0, 0.574, 0.819),
        float3(-0.574, 0.0, 0.819),
        float3(0.0, -0.574, 0.819),
        float3(0.641, 0.641, 0.423),
        float3(-0.641, 0.641, 0.423),
        float3(-0.641, -0.641, 0.423),
        float3(0.641, -0.641, 0.423),
    };

    const float kTanHalf = 0.55;   // ~30 deg cone half-angle
    const float kBias = 0.15;      // lift off the surface to skip self-intersection
    const float kStart = 0.1;
    const float kStepFloor = 0.05;
    const float kMaxLeap = 4.0;
    const float kMaxDist = 8.0;    // DFAO sky-occlusion range (world metres)
    const int kSteps = 16;

    float3 origin = worldPos + N * kBias;
    float visSum = 0.0;
    float weightSum = 0.0;
    for (int c = 0; c < 9; c = c + 1)
    {
        float3 dir = normalize(kCones[c].x * T + kCones[c].y * B + kCones[c].z * N);
        float w = max(dot(dir, N), 0.0);
        if (w <= 0.0) { continue; }
        float minVis = 1.0;
        float t = kStart;
        for (int i = 0; i < kSteps; i = i + 1)
        {
            bool valid;
            float d = gdfDistance(origin + dir * t, valid);
            // Left the cascade coverage: open sky beyond, so this cone keeps its accumulated
            // openness and stops marching.
            if (!valid) { break; }
            float coneRadius = kTanHalf * t;
            // Penumbra only: negative interior distances clamp to 0 (fully blocked), never
            // brightening; the smallest ratio along the cone is its open fraction.
            minVis = min(minVis, saturate(d / max(coneRadius, 1e-4)));
            if (minVis <= 0.0) { break; }
            t += clamp(d, kStepFloor, kMaxLeap);
            if (t > kMaxDist) { break; }
        }
        visSum += w * minVis;
        weightSum += w;
    }
    return weightSum > 0.0 ? saturate(visSum / weightSum) : 1.0;
}
