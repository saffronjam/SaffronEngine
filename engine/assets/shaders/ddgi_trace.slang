// DDGI ray trace (software): one thread per (probe, ray). Sphere-marches a ray from the probe
// center through the REAL distance field — the per-mesh MDF for the near field (~first 2 m) and
// the camera-centered Global Distance Field beyond (the shared `sdf` module's `sampleField`, the
// same near/far handoff the per-pixel cone trace uses). The sky enters radiance ONLY on a ray
// miss; a hit reads the lite per-cell albedo cache (a flat per-cell base color) lit by a crude
// sun+sky term PLUS last-frame probe irradiance re-sampled at the hit (free multi-bounce). The
// sun half of that direct term is NOT unconditional: it is gated by the surface NdotL (the field
// gradient gives the normal) and a shadow sphere-march toward the sun, so a hit inside an enclosed
// interior — where the sun is occluded — receives no sun and the interior goes dark by
// construction, not merely dimmer. The sky half stays a crude uniform ambient (the fidelity cap).
//
// Probes live in a camera-centered scrolling clipmap: the physical atlas tile is a toroidal
// (wrap-modulo) function of the absolute probe cell, so a recenter keeps every staying probe's
// tile + history and only scrolls in the leading slab. The thread's `tid.y + offset` is the
// PHYSICAL probe tile (the round-robin budget slice); its logical position within the volume is
// `wrapMod(physical - scrollBase, count)`, which gives its world position.
//
// Sets: 0 = the bindless brick arrays (the `sdf`/`mdf_brick` import), 1 = the light set (the
// per-mesh SDF instance list at binding 8, the GDF cascade clipmap + params at 9/10), 2 = the
// DDGI trace set (b0 = albedo cache Sampler3D, b1 = prev-irradiance atlas Sampler2D, b2 = ray
// image rgba16f storage, b3 = raw-radiance sky SH). Push = probe grid + volume + sun + scroll/frame.

import sdf;
import sky_sh;

[[vk::binding(0, 2)]] Sampler3D albedoCache;
[[vk::binding(1, 2)]] Sampler2D irradianceAtlas;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 2)]] RWTexture2D<float4> rayOut;
[[vk::binding(3, 2)]] StructuredBuffer<float4> skyShCoefficients;

struct Push
{
    uint4 probeCount;     // xyz = probes per axis, w = rays per probe
    float4 volumeMin;     // xyz = volume min corner (world), w = irradiance tile interior
    float4 volumeExtent;  // xyz = volume size (world) = count * spacing, w reserved
    float4 sunDir;        // xyz = direction the sun travels, w = sun intensity
    float4 sunColor;      // rgb sun color, w = frame index (for ray rotation)
    float4 budgetOffset;  // x = round-robin probe-budget offset
    int4 scrollBase;      // xyz = toroidal scroll base (wrapMod(snapBase, count)), w reserved
};
[[vk::push_constant]] Push pc;

static const float PI = 3.14159265;

// Euclidean modulo (HLSL `%` truncates; a negative physical-minus-scroll must wrap positive).
int wrapMod(int v, int n)
{
    int r = v % n;
    return (r < 0) ? r + n : r;
}

// Fibonacci-sphere direction i of n, rotated by a per-frame golden offset.
float3 sphericalFibonacci(uint i, uint n, float rot)
{
    float phi = 2.0 * PI * frac(float(i) * 0.618033988749895 + rot);
    float cosT = 1.0 - (2.0 * float(i) + 1.0) / float(n);
    float sinT = sqrt(max(0.0, 1.0 - cosT * cosT));
    return float3(cos(phi) * sinT, sin(phi) * sinT, cosT);
}

// World position of the probe at LOCAL index `l` (cell-centered within the camera-centered box).
float3 probeWorldPos(uint3 l)
{
    float3 t = (float3(l) + 0.5) / float3(pc.probeCount.xyz);
    return pc.volumeMin.xyz + t * pc.volumeExtent.xyz;
}

// Sample a probe's previous irradiance in direction `dir` from the atlas (with gutter). `tile3d`
// is the PHYSICAL atlas tile (storage coordinate), matching the blend's tile linearization.
float3 sampleProbeIrradiance(uint3 tile3d, float3 dir, uint interior)
{
    uint tile = interior + 2;  // interior + 1-texel gutter each side
    uint tilesX = pc.probeCount.x * pc.probeCount.y;
    uint tileX = tile3d.x + tile3d.y * pc.probeCount.x;
    uint tileY = tile3d.z;
    float2 oct = octEncode(normalize(dir));
    float2 inner = (oct * float(interior) + 1.0);  // into [1, tile-1]
    float2 atlasPx = float2(tileX, tileY) * float(tile) + inner;
    uint atlasW = tilesX * tile;
    uint atlasH = pc.probeCount.z * tile;
    float2 uv = (atlasPx + 0.5) / float2(atlasW, atlasH);
    return irradianceAtlas.SampleLevel(uv, 0.0).rgb;
}

// The lite per-cell albedo cache: a flat per-cell base color aligned to the GDF finest cascade
// (cascade 0, toroidal `wp / worldExtent` with a repeat sampler). `.a` is the written flag (the
// composite writes alpha 1 for covered cells, 0 for empty); a miss / out-of-cascade falls back
// to a neutral mid-gray so DDGI without the GDF (or beyond the cache) still bounces a plausible
// hue. This is the documented fidelity cap — no normal, no view-dependent shading, no emissive.
float3 sampleAlbedo(float3 wp)
{
    const float3 kFallback = float3(0.5, 0.5, 0.5);
    if (!gdfEnabled()) { return kFallback; }
    float4 a = albedoCache.SampleLevel(wp / gdfFinestWorldExtent(), 0.0);
    return a.a > 0.5 ? a.rgb : kFallback;
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint ray = tid.x;
    uint total = pc.probeCount.x * pc.probeCount.y * pc.probeCount.z;
    // Round-robin probe budget: the dispatch covers only DDGI_PROBE_BUDGET probes, starting at
    // the budget offset and wrapping, so the whole volume refreshes over several frames.
    uint physIndex = (tid.y + uint(pc.budgetOffset.x)) % total;
    if (ray >= pc.probeCount.w)
    {
        return;
    }
    uint3 phys;
    phys.x = physIndex % pc.probeCount.x;
    phys.y = (physIndex / pc.probeCount.x) % pc.probeCount.y;
    phys.z = physIndex / (pc.probeCount.x * pc.probeCount.y);

    // Physical tile -> logical cell within the volume (the inverse of the toroidal scroll fold),
    // which gives the probe's world position this frame.
    uint3 lcl = uint3(
        uint(wrapMod(int(phys.x) - pc.scrollBase.x, int(pc.probeCount.x))),
        uint(wrapMod(int(phys.y) - pc.scrollBase.y, int(pc.probeCount.y))),
        uint(wrapMod(int(phys.z) - pc.scrollBase.z, int(pc.probeCount.z))));

    float3 origin = probeWorldPos(lcl);
    float rot = frac(pc.sunColor.w * 0.618033988749895);
    float3 dir = sphericalFibonacci(ray, pc.probeCount.w, rot);

    uint count = sdfInstanceCount();
    uint interior = uint(pc.volumeMin.w);  // irradiance octahedral tile interior

    // Sphere-march the unified field (near MDF -> far GDF). A hit is the field distance dropping
    // below the surface epsilon; advance by the field distance (clamped), cap by march length +
    // a step budget. Empty space leaps by the conservative coverage/GDF distance.
    const float kStart = 0.3;        // start off the probe center to avoid a self-hit
    const float kSurfEps = 0.1;      // a surface within 10 cm is a hit
    const float kMinStep = 0.05;
    const float kMaxLeap = 4.0;
    const float kMaxDist = 40.0;
    const int kSteps = 64;
    const float kGradEps = 0.08;     // forward-difference offset for the hit-surface normal
    const float kShadowBias = 0.1;   // start the shadow march off the surface to avoid a self-hit
    const float kShadowMaxDist = 20.0;
    const int kShadowSteps = 24;

    float3 radiance = float3(0.0);
    float hitDist = kMaxDist;
    bool hit = false;
    // Beer-Lambert transmittance through porous aggregate matter (canopies): the ray dims
    // instead of stopping, and saturated extinction inside dense matter is itself a hit.
    float transmittance = 1.0;
    const float kAggregateHit = 0.05;
    float t = kStart;
    for (int s = 0; s < kSteps; s = s + 1)
    {
        float3 wp = origin + dir * t;
        float footprint = max(kMinStep, t * 0.03);
        SdfSample smp = sampleField(wp, count, footprint, t);
        if (smp.distance < kSurfEps)
        {
            // Hit a true distance-field surface. Radiance = albedo * shadowed sun direct +
            // last-frame probe irradiance bounce (multi-bounce). NO flat sky term is added at the
            // hit: the sky enters radiance ONLY where a ray MISSES to open space (below), so an
            // enclosed interior surface is lit purely by the sun it can see plus light that bounces
            // in through the openings — a bright skybox therefore cannot flood a sealed room.
            float3 albedo = sampleAlbedo(wp);

            // Surface normal from the field gradient (forward differences off the hit distance).
            float d0 = smp.distance;
            float3 grad = float3(
                sampleField(wp + float3(kGradEps, 0.0, 0.0), count, kGradEps, t).distance,
                sampleField(wp + float3(0.0, kGradEps, 0.0), count, kGradEps, t).distance,
                sampleField(wp + float3(0.0, 0.0, kGradEps), count, kGradEps, t).distance) - d0;
            float3 nrm = (dot(grad, grad) > 1e-10) ? normalize(grad) : -dir;

            // Shadowed sun direct: NdotL toward the sun, killed if a shadow march hits an occluder.
            float3 toSun = -normalize(pc.sunDir.xyz);
            float ndotl = max(0.0, dot(nrm, toSun));
            float sunVis = 0.0;
            if (ndotl > 0.0)
            {
                float st = kStart;
                bool occluded = false;
                float sunTrans = 1.0;
                for (int ss = 0; ss < kShadowSteps; ss = ss + 1)
                {
                    float3 sp = wp + nrm * kShadowBias + toSun * st;
                    SdfSample shadowSmp = sampleField(sp, count, max(kMinStep, st * 0.03), st);
                    if (shadowSmp.distance < kSurfEps) { occluded = true; break; }
                    float sstep = clamp(shadowSmp.distance, kMinStep, kMaxLeap);
                    // A canopy between the surface and the sun dims rather than blocks.
                    sunTrans *= sdfExtinctionStep(shadowSmp.occupancy, sstep);
                    st += sstep;
                    if (sunTrans < 0.02 || st >= kShadowMaxDist) { break; }
                }
                sunVis = occluded ? 0.0 : ndotl * sunTrans;
            }

            float3 directLit = albedo * (pc.sunColor.rgb * pc.sunDir.w * sunVis);
            // Multi-bounce: gather this probe's previous irradiance in the ray direction. This is
            // the sole path by which sky light reaches an interior surface — light that entered
            // through an opening (a missed ray → sky) is stored in probe irradiance and re-gathered
            // here, propagating one bounce deeper per frame.
            float3 bounce = sampleProbeIrradiance(phys, dir, interior) * albedo * 0.5;
            radiance = (directLit + bounce) * transmittance;
            hitDist = t;
            hit = true;
            break;
        }
        float step = clamp(smp.distance, kMinStep, kMaxLeap);
        transmittance *= sdfExtinctionStep(smp.occupancy, step);
        if (transmittance < kAggregateHit)
        {
            // The ray saturated inside a dense aggregate: shade the interior as a hit —
            // the cell's cached colour, an occupancy-gradient normal, and the sun dimmed
            // by the canopy above it.
            float3 albedo = sampleAlbedo(wp);
            float o0 = gdfOccupancyAt(wp);
            float3 grad = float3(
                gdfOccupancyAt(wp + float3(kGradEps, 0.0, 0.0)),
                gdfOccupancyAt(wp + float3(0.0, kGradEps, 0.0)),
                gdfOccupancyAt(wp + float3(0.0, 0.0, kGradEps))) - o0;
            // Density rises toward the interior; the outward normal opposes the gradient.
            float3 nrm = (dot(grad, grad) > 1e-10) ? normalize(-grad) : -dir;
            float3 toSun = -normalize(pc.sunDir.xyz);
            float ndotl = max(0.0, dot(nrm, toSun));
            float sunThrough = 1.0;
            {
                float st = kStart;
                for (int ss = 0; ss < 8; ss = ss + 1)
                {
                    float3 sp = wp + toSun * st;
                    float so = gdfOccupancyAt(sp);
                    float sstep = 1.0;
                    sunThrough *= sdfExtinctionStep(so, sstep);
                    st += sstep;
                    if (sunThrough < 0.02 || st > 8.0) { break; }
                }
            }
            float3 directLit = albedo * (pc.sunColor.rgb * pc.sunDir.w * ndotl * sunThrough);
            float3 bounce = sampleProbeIrradiance(phys, dir, interior) * albedo * 0.5;
            radiance = directLit + bounce;
            hitDist = t;
            hit = true;
            break;
        }
        t += step;
        if (t >= kMaxDist) { break; }
    }
    if (!hit)
    {
        // The ray escaped to open space: it sees the sky through whatever porous matter it
        // crossed (the only place sky enters radiance).
        radiance = skyShRadiance(dir, skyShCoefficients) * transmittance;
    }
    rayOut[uint2(ray, physIndex)] = float4(radiance, hitDist);
}
