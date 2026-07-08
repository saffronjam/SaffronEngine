// Per-unique-edge tessellation-factor kernel (Phase 3a / Phase 10). One compute thread per unique base
// edge of a displaced instance. Writes ONE fractional factor per edge into the instance's slice of the
// factor buffer, so both triangles sharing an edge index the SAME slot and read a bit-identical value —
// the structural (not floating-point) guarantee that a shared edge cannot crack.
//
// The factor is a pure function of (endpoint A, endpoint B, camera, the shared min/max pyramid): the two
// welded endpoint positions + UVs (from the Phase-2 conditioning `welded` buffer, indexed by the edge's
// welded ids) and the pushed camera. An angular arc metric (rotation-stable) drives the base pixel
// budget; a world/local-space fallback keeps it finite and range-clamped when the arc degenerates (an
// endpoint clipped behind the near plane), so the value is never NaN and always in [minFactor, factorCap].
//
// Per-region displacement adaptivity (Phase 10): the displaced surface spans the base edge PLUS the LOCAL
// height range over the edge's UV span, so a surface refines where the height field is *busy*, not just
// where it is globally tall. The local range is sampled from the per-height min/max pyramid (bindless
// binding 4, R32G32_SFLOAT: min in R, max in G) at a mip covering the edge's UV span, point-filtered so
// the conservative bound survives. Because it reads only the two shared endpoint UVs (one agreed
// representative per welded vertex) + the shared pyramid, a shared edge stays bit-identical → crack-free.
//
// The metric is computed in the instance's LOCAL space (endpoints are local; `camPosLocal` is the world
// camera transformed by the inverse model on the CPU). This is exact for uniform scale; non-uniform
// scale mildly distorts the angle — an accepted v1 compromise, documented in the plan. The full
// fractional value is stored (integer part drives the dice grid; the remainder feeds Phase-5 geomorph).

// Set 0 — the shared bindless table (matches the übershader). The factor kernel reads only the per-height
// min/max pyramid array (binding 4), indexed by the height map's bindless slot (`heightIndex`), point-
// sampled with explicit LOD.
[[vk::binding(4, 0)]] Sampler2D heightMinMaxTextures[1024];

// Set 1 — the per-instance edge buffers.
// WeldedVertex: 64 B — position@0 (float4, xyz), direction@16 (float4), tangent@32 (float4), uv@48
// (float4, xy). Read as raw bytes so the std430 packing matches the engine `#[repr(C)]` layout exactly.
[[vk::binding(0, 1)]] ByteAddressBuffer welded;    // WeldedVertex[] (64 B stride)
// Edge: 16 B — v0@0 (uint), v1@4 (uint), flags@8 (uint), pad@12.
[[vk::binding(1, 1)]] ByteAddressBuffer edges;     // Edge[] (16 B stride)
[[vk::binding(2, 1)]] RWStructuredBuffer<float> outFactors; // one factor per edge, at factorBase + edge

static const uint WeldedStride = 64u;
static const uint UvOffset = 48u;
static const uint EdgeStride = 16u;

struct Push
{
    float4x4 mvp;         // view_proj * model — for the near-plane clip test
    float4 camPosLocal;   // world camera in this instance's local space (xyz); w = material uv tiling.x
    float2 viewport;      // render target extent in pixels
    float tanHalfFovY;    // tan(0.5 * vertical FOV)
    float nearPlane;      // camera near distance (clip.w guard)
    float edgeLengthTarget; // desired pixels per micro-edge
    float factorCap;      // hard per-edge upper clamp
    float minFactor;      // lower clamp (>= 1)
    uint edgeCount;       // unique edges in this instance's mesh
    uint factorBase;      // this instance's base offset into outFactors
    float dispAmpLocal;   // LOCAL-space displacement amplitude (the material height_scale) for a full
                          // [0,1] height range; the per-edge local range scales it
    uint heightIndex;     // bindless slot of the height map (addresses its min/max pyramid at binding 4)
    float uvTilingY;      // material uv tiling.y (x rides camPosLocal.w) — the pyramid span is in
                          // TEXTURE space, so a tiled material's edge sweeps tiling× more texels
};
[[vk::push_constant]] Push push;

float3 weldedPosition(uint id)
{
    return welded.Load<float3>(id * WeldedStride);
}

float2 weldedUv(uint id)
{
    return welded.Load<float2>(id * WeldedStride + UvOffset);
}

// The local displacement RANGE over the edge's UV span, sampled from the per-height min/max pyramid: the
// conservative (min, max) over the region between the two endpoint UVs, read at a mip whose one texel
// covers the span so a coarse edge still bounds the whole field it crosses. Point-filtered (the sampler
// is nearest) so min is not blended into max. Returns the [0,1] range (max - min); 0 in a flat region.
float localHeightRange(float2 uv0, float2 uv1)
{
    uint tw, th;
    heightMinMaxTextures[NonUniformResourceIndex(push.heightIndex)].GetDimensions(tw, th);
    // TEXTURE-space span: the material's uv tiling multiplies how many texels the edge sweeps (offset
    // shifts where, not how much — omitted; the pyramid sampler wraps, so tiled coords land correctly).
    const float2 tiling = float2(push.camPosLocal.w, push.uvTilingY);
    const float uvSpan = length((uv1 - uv0) * tiling);
    const float lod = log2(max(uvSpan * (float)tw, 1.0));
    const float2 s0 = heightMinMaxTextures[NonUniformResourceIndex(push.heightIndex)].SampleLevel(uv0 * tiling, lod).rg;
    const float2 s1 = heightMinMaxTextures[NonUniformResourceIndex(push.heightIndex)].SampleLevel(uv1 * tiling, lod).rg;
    const float lo = min(s0.x, s1.x);
    const float hi = max(s0.y, s1.y);
    return max(hi - lo, 0.0);
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    const uint e = tid.x;
    if (e >= push.edgeCount)
    {
        return;
    }

    const uint eBase = e * EdgeStride;
    const uint v0 = edges.Load<uint>(eBase);
    const uint v1 = edges.Load<uint>(eBase + 4u);
    const float3 p0 = weldedPosition(v0);
    const float3 p1 = weldedPosition(v1);

    // Near-plane clip guard: if either endpoint is behind the near plane, the screen-space arc is
    // undefined — take the finite fallback instead.
    const float4 c0 = mul(push.mvp, float4(p0, 1.0));
    const float4 c1 = mul(push.mvp, float4(p1, 1.0));
    const bool clipped = c0.w <= push.nearPlane || c1.w <= push.nearPlane;

    // Edge midpoint distance — used for both the fallback arc and the displacement-amplitude projection.
    const float3 mid = 0.5 * (p0 + p1);
    const float dist = max(length(mid - push.camPosLocal.xyz), 1e-4);

    float pixels;
    if (clipped)
    {
        // World/local-space fallback (depends only on the two endpoints + camera): local edge length
        // over the camera distance, converted to an approximate pixel budget. Symmetric in p0/p1.
        const float len = length(p1 - p0);
        const float halfAngle = atan(len * 0.5 / dist);
        pixels = (2.0 * halfAngle) / max(push.tanHalfFovY, 1e-4) * push.viewport.y * 0.5;
    }
    else
    {
        // Angular arc the edge subtends at the camera — order-independent (dot is symmetric), stable
        // under camera rotation (unlike post-projection screen length).
        const float3 d0 = normalize(p0 - push.camPosLocal.xyz);
        const float3 d1 = normalize(p1 - push.camPosLocal.xyz);
        const float arc = acos(clamp(dot(d0, d1), -1.0, 1.0));
        pixels = arc / max(push.tanHalfFovY, 1e-4) * push.viewport.y * 0.5;
    }

    // Displacement-aware term (Phase 10, per-region): the displaced surface spans the base edge PLUS the
    // LOCAL height range over this edge's UV span, so a flat base under a *busy* (not just tall) height
    // field refines automatically while a genuinely smooth region stays coarse. The local range scales
    // the full local amplitude (`dispAmpLocal`); project it at the midpoint distance (small-angle form)
    // and drive the factor by whichever extent is larger. A pure function of the shared endpoints (UVs +
    // positions) + the shared pyramid, so a shared edge stays bit-identical — still crack-free.
    const float localRange = localHeightRange(weldedUv(v0), weldedUv(v1));
    const float dispAmp = localRange * push.dispAmpLocal;
    const float dispPixels = (dispAmp / dist) / max(push.tanHalfFovY, 1e-4) * push.viewport.y * 0.5;
    const float driving = max(pixels, dispPixels);
    const float f = clamp(driving / max(push.edgeLengthTarget, 1e-4), push.minFactor, push.factorCap);
    outFactors[push.factorBase + e] = f;
}
