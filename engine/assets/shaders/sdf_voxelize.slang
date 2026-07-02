// GPU mesh-distance-field bake, pass 1 of 2: seed scatter. One thread per triangle.
//
// Each thread walks the fine voxels in its triangle's AABB (expanded by a small halo) and,
// for every voxel within a few cells of the triangle, atomically keeps the *nearest*
// triangle by min-ing a packed (quantizedDistance << 20 | triangleIndex) key into the
// seed-key image. A later pass (sdf_jfa, init mode) decodes the winning triangle per voxel
// and recomputes the exact closest surface point + normal — so the seed is race-free
// (the key carries both the ordering distance and the triangle, and only the per-voxel
// resolve writes the float seed). The jump-flood then propagates the nearest seed to every
// voxel. Set 0: b0 positions (float4), b1 indices (uint), b2 seed-key image (r32ui).

[[vk::binding(0, 0)]] StructuredBuffer<float4> positions; // xyz vertex positions (local space)
[[vk::binding(1, 0)]] StructuredBuffer<uint> indices;     // triangle index stream
[[vk::image_format("r32ui")]]
[[vk::binding(2, 0)]] RWTexture3D<uint> seedKey;          // (quantDist<<20 | triIndex), min-combined

struct Push
{
    uint4 dims;       // xyz = fine voxel dims, w = triangle count
    float4 boundsMin; // xyz = grid lower corner (local), w = maxDist (unused here)
    float4 cell;      // xyz = per-axis cell size, w = winding (unused here)
    int4 misc;        // unused here (jfa step / parity / mode)
};
[[vk::push_constant]] Push pc;

// Index field gets the low 20 bits (≤ ~1M triangles); the distance quantizes into the top
// 12 bits, so InterlockedMin picks the nearest triangle (ties broken by lower index).
static const uint kIndexBits = 20u;
static const uint kIndexMask = (1u << kIndexBits) - 1u;
static const uint kDistLevels = (1u << (32u - kIndexBits)) - 1u;
// Seed a halo of this many cells around each triangle; the jump-flood fills the rest.
static const float kSeedRadiusCells = 3.0;

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

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    uint t = tid.x;
    if (t >= pc.dims.w) { return; }

    float3 a = positions[indices[t * 3u + 0u]].xyz;
    float3 b = positions[indices[t * 3u + 1u]].xyz;
    float3 c = positions[indices[t * 3u + 2u]].xyz;

    int3 dims = int3(pc.dims.xyz);
    float3 cell = pc.cell.xyz;
    float3 origin = pc.boundsMin.xyz;
    float maxCell = max(cell.x, max(cell.y, cell.z));
    float seedMax = maxCell * kSeedRadiusCells;
    float quantScale = float(kDistLevels) / seedMax;

    // The triangle's voxel-index AABB (continuous voxel coord = (p - origin)/cell - 0.5),
    // expanded by the seed halo and clamped to the grid.
    float3 triMin = min(a, min(b, c));
    float3 triMax = max(a, max(b, c));
    float3 gMin = (triMin - origin) / cell - 0.5 - kSeedRadiusCells;
    float3 gMax = (triMax - origin) / cell - 0.5 + kSeedRadiusCells;
    int3 lo = clamp(int3(floor(gMin)), int3(0), dims - 1);
    int3 hi = clamp(int3(ceil(gMax)), int3(0), dims - 1);

    for (int z = lo.z; z <= hi.z; z = z + 1)
    {
        for (int y = lo.y; y <= hi.y; y = y + 1)
        {
            for (int x = lo.x; x <= hi.x; x = x + 1)
            {
                float3 center = origin + (float3(x, y, z) + 0.5) * cell;
                float3 q = closestPointOnTriangle(center, a, b, c);
                float dist = length(center - q);
                if (dist > seedMax) { continue; }
                uint quant = min(uint(dist * quantScale), kDistLevels);
                uint key = (quant << kIndexBits) | (t & kIndexMask);
                InterlockedMin(seedKey[uint3(x, y, z)], key);
            }
        }
    }
}
