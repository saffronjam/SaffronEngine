// Blend traced ray hit-distances into the moment (distance) atlas for the Chebyshev
// visibility test that kills DDGI light leaking. Like ddgi_blend_irradiance but: stores
// mean distance (r) + mean squared distance (r^2) in RG, a sharper cosine power, and the
// per-probe tile is larger (16x16 interior). Set 0: b0 = ray image (sampler), b1 = moment
// atlas (rg16f storage). Push = probe grid + tile + hysteresis + max distance + scroll geometry.

[[vk::binding(0, 0)]] Sampler2D rayImage;
[[vk::image_format("rg16f")]]
[[vk::binding(1, 0)]] RWTexture2D<float2> atlas;

struct Push
{
    uint4 probeCount;  // xyz = probes per axis, w = rays per probe
    uint4 tile;        // x = interior size, y = 1 if whole-volume first frame, z = round-robin offset, w = probe budget
    float4 params;     // x = hysteresis, y = max distance, z = ray-set golden rotation
    int4 scrollBase;   // xyz = toroidal scroll base, w = total probe count
    int4 delta;        // xyz = scroll accumulated over one round-robin cycle, w reserved
};
[[vk::push_constant]] Push pc;

static const float PI = 3.14159265;

int wrapMod(int v, int n)
{
    int r = v % n;
    return (r < 0) ? r + n : r;
}

// Whether physical tile `phys` was retraced this frame (its rays match the current rotation). Only
// these probes may be re-integrated; untraced probes keep their last moments (see ddgi_blend_irradiance).
bool probeTraced(uint3 phys)
{
    uint total = uint(pc.scrollBase.w);
    uint physIndex = phys.x + phys.y * pc.probeCount.x + phys.z * pc.probeCount.x * pc.probeCount.y;
    return uint(wrapMod(int(physIndex) - int(pc.tile.z), int(total))) < pc.tile.w;
}

// Whether physical tile `phys` (3D) resets its history this frame — the whole-volume first frame,
// or a probe newly-exposed since it was last re-rayed. `delta` accumulates the scroll over one full
// round-robin cycle (see ddgi_blend_irradiance). Reached only for a traced probe.
bool probeReset(uint3 phys)
{
    if (pc.tile.y != 0u) { return true; }
    int3 count = int3(pc.probeCount.xyz);
    int3 lcl = int3(wrapMod(int(phys.x) - pc.scrollBase.x, count.x),
                    wrapMod(int(phys.y) - pc.scrollBase.y, count.y),
                    wrapMod(int(phys.z) - pc.scrollBase.z, count.z));
    int3 g = pc.delta.xyz + lcl;
    bool newlyExposed = any(g < int3(0)) || any(g >= count);
    return newlyExposed;
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint interior = pc.tile.x;
    uint full = interior + 2;
    uint tilesX = pc.probeCount.x * pc.probeCount.y;
    uint atlasW = tilesX * full;
    uint atlasH = pc.probeCount.z * full;
    if (tid.x >= atlasW || tid.y >= atlasH)
    {
        return;
    }
    uint2 local = uint2(tid.x % full, tid.y % full);
    if (local.x == 0 || local.y == 0 || local.x > interior || local.y > interior)
    {
        return;
    }
    uint tileX = tid.x / full;
    uint tileY = tid.y / full;
    uint3 phys = uint3(tileX % pc.probeCount.x, tileX / pc.probeCount.x, tileY);
    uint probeIndex = phys.x + phys.y * pc.probeCount.x + phys.z * pc.probeCount.x * pc.probeCount.y;
    uint total = pc.probeCount.x * pc.probeCount.y * pc.probeCount.z;
    if (probeIndex >= total)
    {
        return;
    }
    // Skip probes not retraced this frame — re-integrating their stale rays against the current
    // rotation is the moment-atlas wobble that swims the Chebyshev leak test.
    if (!probeTraced(phys))
    {
        return;
    }

    float2 e = ((float2(local) - 0.5) / float(interior)) * 2.0 - 1.0;
    float3 texelDir = float3(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    if (texelDir.z < 0.0)
    {
        texelDir.xy = (1.0 - abs(texelDir.yx)) * float2(texelDir.x >= 0.0 ? 1.0 : -1.0, texelDir.y >= 0.0 ? 1.0 : -1.0);
    }
    texelDir = normalize(texelDir);

    float maxDist = pc.params.y;
    float2 moments = float2(0.0);
    float weightSum = 0.0;
    uint rays = pc.probeCount.w;
    float2 rayImgSize = float2(rays, total);
    for (uint r = 0; r < rays; r = r + 1)
    {
        float2 ruv = (float2(r, probeIndex) + 0.5) / rayImgSize;
        float dist = min(rayImage.SampleLevel(ruv, 0.0).a, maxDist);
        // Match the trace's rotated Fibonacci directions (see ddgi_blend_irradiance).
        float phi = 2.0 * PI * frac(float(r) * 0.618033988749895 + pc.params.z);
        float cosT = 1.0 - (2.0 * float(r) + 1.0) / float(rays);
        float sinT = sqrt(max(0.0, 1.0 - cosT * cosT));
        float3 rayDir = float3(cos(phi) * sinT, sin(phi) * sinT, cosT);
        float w = pow(max(0.0, dot(texelDir, rayDir)), 50.0);  // sharp for distance
        moments += float2(dist, dist * dist) * w;
        weightSum += w;
    }
    if (weightSum > 1e-6)
    {
        moments /= weightSum;
    }
    else
    {
        moments = float2(maxDist, maxDist * maxDist);
    }

    uint2 px = uint2(tid.xy);
    float2 prev = atlas[px];
    float hysteresis = probeReset(phys) ? 0.0 : pc.params.x;
    atlas[px] = lerp(moments, prev, hysteresis);
}
