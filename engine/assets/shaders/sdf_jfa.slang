// GPU mesh-distance-field bake, pass 2 of 2: jump-flood. One thread per fine voxel.
//
// Two modes (push.misc.z):
//   1 init — decode the winning triangle the scatter (sdf_voxelize) left in the seed-key
//            image, recompute the exact closest surface point + normal at this voxel, and
//            write it (or a sentinel for an unseeded voxel) into seed buffer A.
//   0 prop — one jump-flood step of size `push.misc.x`: sample the 27 neighbours at that
//            offset, keep the seed whose stored surface position is nearest this voxel's
//            centre, and write it to the opposite ping-pong buffer (parity in push.misc.y).
// Run init once, then prop with steps N/2, N/4, … 1. The seed stores the surface position
// in xyz and the octahedral-packed nearest-triangle normal in w (carried along so the sign
// pass can orient the distance). Set 0: b0 positions, b1 indices, b2 seed-key, b3 seedA,
// b4 seedB (all rgba32f storage for the seeds).

[[vk::binding(0, 0)]] StructuredBuffer<float4> positions;
[[vk::binding(1, 0)]] StructuredBuffer<uint> indices;
[[vk::image_format("r32ui")]]
[[vk::binding(2, 0)]] RWTexture3D<uint> seedKey;
[[vk::image_format("rgba32f")]]
[[vk::binding(3, 0)]] RWTexture3D<float4> seedA;
[[vk::image_format("rgba32f")]]
[[vk::binding(4, 0)]] RWTexture3D<float4> seedB;

struct Push
{
    uint4 dims;       // xyz = fine voxel dims, w = triangle count
    float4 boundsMin; // xyz = grid lower corner (local), w = maxDist
    float4 cell;      // xyz = per-axis cell size, w = winding
    int4 misc;        // x = jump step, y = parity (0/1), z = mode (1 init, 0 prop)
};
[[vk::push_constant]] Push pc;

static const uint kIndexBits = 20u;
static const uint kIndexMask = (1u << kIndexBits) - 1u;
static const float kSentinel = 1e30;

// Octahedral encode/decode of a unit vector (resource-free; shared with the AO accumulator).
float2 octEncode(float3 n)
{
    n /= (abs(n.x) + abs(n.y) + abs(n.z));
    float2 e = n.xy;
    if (n.z < 0.0)
    {
        e = (1.0 - abs(e.yx)) * float2(e.x >= 0.0 ? 1.0 : -1.0, e.y >= 0.0 ? 1.0 : -1.0);
    }
    return e;
}

float packNormal(float3 n)
{
    float2 e = octEncode(normalize(n)) * 0.5 + 0.5;
    uint2 q = uint2(round(saturate(e) * 65535.0));
    return asfloat(q.x | (q.y << 16));
}

float3 closestPointOnTriangle(float3 p, float3 a, float3 b, float3 c)
{
    float3 ab = b - a;
    float3 ac = c - a;
    float3 ap = p - a;
    float d1 = dot(ab, ap);
    float d2 = dot(ac, ap);
    if (d1 <= 0.0 && d2 <= 0.0) { return a; }
    float3 bp = p - b;
    float d3 = dot(ab, bp);
    float d4 = dot(ac, bp);
    if (d3 >= 0.0 && d4 <= d3) { return b; }
    float vc = d1 * d4 - d3 * d2;
    if (vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0)
    {
        float v = d1 / (d1 - d3);
        return a + v * ab;
    }
    float3 cp = p - c;
    float d5 = dot(ab, cp);
    float d6 = dot(ac, cp);
    if (d6 >= 0.0 && d5 <= d6) { return c; }
    float vb = d5 * d2 - d1 * d6;
    if (vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0)
    {
        float w = d2 / (d2 - d6);
        return a + w * ac;
    }
    float va = d3 * d6 - d5 * d4;
    if (va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0)
    {
        float w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return b + w * (c - b);
    }
    float denom = 1.0 / (va + vb + vc);
    float v = vb * denom;
    float w = vc * denom;
    return a + ab * v + ac * w;
}

float4 readSeed(uint3 c)
{
    return pc.misc.y == 0 ? seedA[c] : seedB[c];
}

void writeSeed(uint3 c, float4 v)
{
    if (pc.misc.y == 0) { seedB[c] = v; }
    else { seedA[c] = v; }
}

[shader("compute")]
[numthreads(4, 4, 4)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    int3 dims = int3(pc.dims.xyz);
    if (any(int3(tid) >= dims)) { return; }
    float3 center = pc.boundsMin.xyz + (float3(tid) + 0.5) * pc.cell.xyz;

    if (pc.misc.z == 1)
    {
        // Init: resolve the winning triangle the scatter left for this voxel.
        uint key = seedKey[tid];
        if (key == 0xFFFFFFFFu)
        {
            seedA[tid] = float4(kSentinel, kSentinel, kSentinel, 0.0);
            return;
        }
        uint t = key & kIndexMask;
        float3 a = positions[indices[t * 3u + 0u]].xyz;
        float3 b = positions[indices[t * 3u + 1u]].xyz;
        float3 c = positions[indices[t * 3u + 2u]].xyz;
        float3 q = closestPointOnTriangle(center, a, b, c);
        float3 n = cross(b - a, c - a);
        seedA[tid] = float4(q, packNormal(n));
        return;
    }

    // Propagate: keep the nearest stored surface position among the 27 neighbours.
    int step = pc.misc.x;
    float4 best = readSeed(tid);
    float bestDist = best.x < kSentinel * 0.5 ? length(center - best.xyz) : kSentinel;
    for (int dz = -1; dz <= 1; dz = dz + 1)
    {
        for (int dy = -1; dy <= 1; dy = dy + 1)
        {
            for (int dx = -1; dx <= 1; dx = dx + 1)
            {
                int3 nc = int3(tid) + int3(dx, dy, dz) * step;
                if (any(nc < 0) || any(nc >= dims)) { continue; }
                float4 cand = readSeed(uint3(nc));
                if (cand.x >= kSentinel * 0.5) { continue; }
                float d = length(center - cand.xyz);
                if (d < bestDist)
                {
                    bestDist = d;
                    best = cand;
                }
            }
        }
    }
    writeSeed(tid, best);
}
