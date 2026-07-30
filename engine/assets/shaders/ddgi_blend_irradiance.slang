// Blend traced ray radiance into the irradiance atlas. One thread per INTERIOR octahedral
// texel of each probe tile: integrate all rays weighted by max(0, dot(texelDir, rayDir)),
// then temporally lerp into the previous atlas value (hysteresis). Set 0: b0 = ray image
// (sampler), b1 = irradiance atlas (rgba16f storage). Push = probe grid + tile + hysteresis +
// the scroll geometry (so a newly-scrolled-in probe resets its history when it is re-rayed).

[[vk::binding(0, 0)]] Sampler2D rayImage;
[[vk::image_format("rgba16f")]]
[[vk::binding(1, 0)]] RWTexture2D<float4> atlas;

struct Push
{
    uint4 probeCount;  // xyz = probes per axis, w = rays per probe
    uint4 tile;        // x = interior size, y = 1 if whole-volume first frame, z = round-robin offset, w = probe budget
    float4 params;     // x = hysteresis (history weight), z = ray-set golden rotation
    int4 scrollBase;   // xyz = toroidal scroll base (wrapMod(snapBase, count)), w = total probe count
    int4 delta;        // xyz = scroll accumulated over one round-robin cycle (snapBase - snapBase a cycle ago), w reserved
};
[[vk::push_constant]] Push pc;

static const float PI = 3.14159265;

int wrapMod(int v, int n)
{
    int r = v % n;
    return (r < 0) ? r + n : r;
}

float3 octDecode(float2 uv)  // [0,1]^2 -> unit dir
{
    float2 e = uv * 2.0 - 1.0;
    float3 v = float3(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    if (v.z < 0.0)
    {
        v.xy = (1.0 - abs(v.yx)) * float2(v.x >= 0.0 ? 1.0 : -1.0, v.y >= 0.0 ? 1.0 : -1.0);
    }
    return normalize(v);
}

// Whether physical tile `phys` (3D) is reset this frame: the whole-volume first frame, or a probe
// that became newly-exposed at any point since it was last re-rayed AND is re-rayed this frame.
// `delta` is the scroll accumulated over one full round-robin cycle (the gap between a probe's
// consecutive traces), so a probe that scrolled in is still flagged newly-exposed on the frame it
// is finally traced — no ghosting trail from probes that scrolled in between budget slices. The
// reset is applied only when the probe is re-rayed (fresh rays for its new position exist), so it
// snaps to correct radiance, never to a scrolled-out probe's stale value.
// Whether physical tile `phys` is in this frame's round-robin trace window — i.e. its rays in the
// ray image were (re)traced THIS frame with the current golden rotation `pc.params.z`. Only these
// probes may be re-integrated + blended; an untraced probe's stored rays belong to a PAST rotation,
// so re-integrating them against the current rotation would wobble its irradiance every frame. The
// blend therefore skips untraced probes and leaves their last correctly-integrated atlas value
// (RTXGI/Majercik: blend only the probes you just traced).
bool probeTraced(uint3 phys)
{
    uint total = uint(pc.scrollBase.w);
    uint physIndex = phys.x + phys.y * pc.probeCount.x + phys.z * pc.probeCount.x * pc.probeCount.y;
    return uint(wrapMod(int(physIndex) - int(pc.tile.z), int(total))) < pc.tile.w;
}

bool probeReset(uint3 phys)
{
    if (pc.tile.y != 0u) { return true; }
    int3 count = int3(pc.probeCount.xyz);
    int3 lcl = int3(wrapMod(int(phys.x) - pc.scrollBase.x, count.x),
                    wrapMod(int(phys.y) - pc.scrollBase.y, count.y),
                    wrapMod(int(phys.z) - pc.scrollBase.z, count.z));
    int3 g = pc.delta.xyz + lcl;  // position relative to the volume one round-robin cycle ago
    bool newlyExposed = any(g < int3(0)) || any(g >= count);
    return newlyExposed;  // reached only for a traced probe (untraced probes return early)
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
    // Which probe tile + the local texel within it; skip gutter texels (filled by border).
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
    // Only probes retraced this frame carry rays for the current rotation; leave the rest untouched
    // (their last blended value is correct — re-integrating stale rays here is the wobble/swim bug).
    if (!probeTraced(phys))
    {
        return;
    }

    float2 octUv = (float2(local) - 0.5) / float(interior);  // texel center -> [0,1]
    float3 texelDir = octDecode(octUv);

    float3 sum = float3(0.0);
    float weightSum = 0.0;
    uint rays = pc.probeCount.w;
    float2 rayImgSize = float2(rays, total);
    for (uint r = 0; r < rays; r = r + 1)
    {
        float2 ruv = (float2(r, probeIndex) + 0.5) / rayImgSize;
        float4 rd = rayImage.SampleLevel(ruv, 0.0);
        // Reconstruct the ray direction the same way the trace did (Fibonacci sphere) — including
        // the per-frame golden rotation, so the radiance stored at the rotated directions is
        // integrated against the directions it was actually traced along.
        float phi = 2.0 * PI * frac(float(r) * 0.618033988749895 + pc.params.z);
        float cosT = 1.0 - (2.0 * float(r) + 1.0) / float(rays);
        float sinT = sqrt(max(0.0, 1.0 - cosT * cosT));
        float3 rayDir = float3(cos(phi) * sinT, sin(phi) * sinT, cosT);
        float w = max(0.0, dot(texelDir, rayDir));
        sum += rd.rgb * w;
        weightSum += w;
    }
    float3 irradiance = weightSum > 0.0 ? sum / weightSum : float3(0.0);

    uint2 px = uint2(tid.xy);
    float3 prev = atlas[px].rgb;
    // First frame OR a freshly-relocated probe drops history; otherwise the temporal hysteresis.
    float hysteresis = probeReset(phys) ? 0.0 : pc.params.x;
    // Adaptive convergence (Majercik/RTXGI): when the freshly-integrated irradiance differs strongly
    // from history — a newly-relevant probe as the camera moves — lower the hysteresis so the probe
    // snaps in over a few frames rather than trailing a flat ~20-frame tail. A luminance-relative
    // delta drives the ramp.
    float lumPrev = dot(prev, float3(0.2126, 0.7152, 0.0722));
    float lumNew = dot(irradiance, float3(0.2126, 0.7152, 0.0722));
    float rel = abs(lumNew - lumPrev) / max(max(lumPrev, lumNew), 1e-4);
    if (rel > 0.80) { hysteresis = 0.0; }
    else if (rel > 0.25) { hysteresis = max(0.0, hysteresis - 0.15); }
    float3 blended = lerp(irradiance, prev, hysteresis);
    atlas[px] = float4(blended, 1.0);
}
