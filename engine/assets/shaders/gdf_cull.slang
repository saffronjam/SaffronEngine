// Global-SDF cull: bins the per-mesh MDF instances into the cascade(s) their world AABB touches.
// One thread per SDF instance; each tests its world AABB against every cascade's world bounds and
// atomic-appends its index into that cascade's segment of the compacted cull list. The composite
// then iterates only the surviving instances per cascade, so the per-voxel `min()` cost no longer
// scans the whole scene. The bindless brick arrays ride set 0 (the `mdf_brick` import provides the
// `SdfInstance` struct; the cull reads only the world AABB, not the bricks).

import mdf_brick;

// Set 1, binding 0: the per-mesh SDF-occluder instance list (the same SSBO the lighting cone
// trace reads, bound here for the cull). Binding 1: the compacted cull list — `kGdfCascades`
// atomic counters in the header, then per-cascade segments of `maxCulled` instance indices.
[[vk::binding(0, 1)]] StructuredBuffer<SdfInstance> instances;
[[vk::binding(1, 1)]] RWStructuredBuffer<uint> cullList;
// Binding 2: the occluder scatter's meta words ([0] = instances written this frame).
// The list is GPU-produced, so the count cannot ride the push; `counts.x` bounds it
// as the region capacity instead.
[[vk::binding(2, 1)]] StructuredBuffer<uint> sdfMeta;

// Header words before the per-cascade index segments (the per-cascade atomic counters + pad).
// Matches `global_sdf.rs::GDF_CULL_HEADER`.
static const uint kCullHeader = 4u;

struct Push
{
    float4 cascade[3];  // xyz = cascade world center, w = cascade half-extent (world)
    uint4 counts;       // x = instance capacity, y = max culled per cascade, z = cascade count, w reserved
};
[[vk::push_constant]] Push pc;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint i = tid.x;
    if (i >= min(sdfMeta[0], pc.counts.x)) { return; }

    SdfInstance inst = instances[i];
    float3 wmin = inst.worldMin.xyz;
    float3 wmax = inst.worldMax.xyz;

    uint maxCulled = pc.counts.y;
    uint cascadeCount = min(pc.counts.z, 3u);
    for (uint c = 0; c < cascadeCount; c = c + 1)
    {
        float half = pc.cascade[c].w;
        float3 cmin = pc.cascade[c].xyz - half;
        float3 cmax = pc.cascade[c].xyz + half;
        // AABB overlap: the instance touches this cascade if the boxes intersect on every axis.
        if (all(wmax >= cmin) && all(wmin <= cmax))
        {
            uint slot;
            InterlockedAdd(cullList[c], 1u, slot);
            // Overflow is dropped (the composite reads min(count, maxCulled)); a cascade can hold
            // up to `maxCulled` occluders. Keeps the list bounded without a second pass.
            if (slot < maxCulled)
            {
                cullList[kCullHeader + c * maxCulled + slot] = i;
            }
        }
    }
}
