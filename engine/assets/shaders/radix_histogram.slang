// LSD radix sort, per-pass step 1: per-workgroup 256-bucket histograms over the pair
// keys' current byte. The table is (workgroup, bucket)-indexed; the global scan reads
// it column-major so bucket order dominates and the scatter stays stable.

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<uint2> pairs;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint> histograms;

struct Push
{
    uint shift;
    uint pairCapacity;
    uint workgroups;
    uint reserved;
};
[[vk::push_constant]] Push pc;

groupshared uint localCounts[256];

[shader("compute")]
[numthreads(256, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID, uint3 gid : SV_GroupID, uint3 lid : SV_GroupThreadID)
{
    localCounts[lid.x] = 0u;
    GroupMemoryBarrierWithGroupSync();
    uint total = min(counters[5], pc.pairCapacity);
    if (tid.x < total)
    {
        uint bucket = (pairs[tid.x].x >> pc.shift) & 255u;
        InterlockedAdd(localCounts[bucket], 1u);
    }
    GroupMemoryBarrierWithGroupSync();
    histograms[lid.x * pc.workgroups + gid.x] = localCounts[lid.x];
}
