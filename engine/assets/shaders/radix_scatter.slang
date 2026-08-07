// LSD radix sort, per-pass step 3: stable scatter. Each workgroup recomputes its local
// per-bucket ranks with a shared-memory serial pass (256 elements), then writes each
// pair to its scanned global base plus local rank.

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<uint2> pairsIn;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint2> pairsOut;
[[vk::binding(3, 0)]] StructuredBuffer<uint> histograms;

struct Push
{
    uint shift;
    uint pairCapacity;
    uint workgroups;
    uint reserved;
};
[[vk::push_constant]] Push pc;

groupshared uint bucketOf[256];
groupshared uint rankOf[256];
groupshared uint cursors[256];

[shader("compute")]
[numthreads(256, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID, uint3 gid : SV_GroupID, uint3 lid : SV_GroupThreadID)
{
    uint total = min(counters[5], pc.pairCapacity);
    bool live = tid.x < total;
    bucketOf[lid.x] = live ? (pairsIn[tid.x].x >> pc.shift) & 255u : 0xFFFFFFFFu;
    cursors[lid.x] = 0u;
    GroupMemoryBarrierWithGroupSync();
    // One thread walks the workgroup's elements in order: exact stable local ranks.
    if (lid.x == 0u)
    {
        for (uint element = 0u; element < 256u; ++element)
        {
            uint bucket = bucketOf[element];
            if (bucket != 0xFFFFFFFFu)
            {
                rankOf[element] = cursors[bucket];
                cursors[bucket] += 1u;
            }
        }
    }
    GroupMemoryBarrierWithGroupSync();
    if (live)
    {
        uint bucket = bucketOf[lid.x];
        uint base = histograms[bucket * pc.workgroups + gid.x];
        pairsOut[base + rankOf[lid.x]] = pairsIn[tid.x];
    }
}
