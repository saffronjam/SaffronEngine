// Per-instance finalize, one thread per instance: once the scan has accumulated the instance's exact
// packed vertex/index totals into `instanceCounters`, write its draw seed and RT primitive count. The
// offsets point at the instance's worst-case-reserved slice base, so its packed output is contiguous
// and the one draw covers it exactly. The binner reads the seed to build the row's executor command,
// and owns `firstInstance` (the record index), so this leaves that word zero.

[[vk::binding(0, 0)]] StructuredBuffer<uint> instanceCounters; // [vertTotal, indexTotal] per instance
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> drawSeeds;      // VkDrawIndexedIndirectCommand (5 uints)/inst
[[vk::binding(2, 0)]] RWStructuredBuffer<uint> primCounts;     // triangles per instance (RT build input)

struct Push
{
    uint counterBase;   // = 2 * instance row (into instanceCounters)
    uint seedBase;      // = 5 * instance row (into drawSeeds)
    uint vbBase;        // this instance's reserved vertex slice base
    uint ibBase;        // this instance's reserved index slice base
    uint instanceRow;   // primCounts index (RT prim count slot)
    uint pad0;
    uint pad1;
    uint pad2;
};
[[vk::push_constant]] Push push;

[shader("compute")]
[numthreads(1, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    if (tid.x != 0u)
    {
        return;
    }
    const uint indexTotal = instanceCounters[push.counterBase + 1u];

    // VkDrawIndexedIndirectCommand { indexCount, instanceCount, firstIndex, vertexOffset, firstInstance }.
    drawSeeds[push.seedBase + 0u] = indexTotal;
    drawSeeds[push.seedBase + 1u] = 1u;
    drawSeeds[push.seedBase + 2u] = push.ibBase;
    drawSeeds[push.seedBase + 3u] = push.vbBase; // vertexOffset is signed; the base is always >= 0
    drawSeeds[push.seedBase + 4u] = 0u;

    primCounts[push.instanceRow] = indexTotal / 3u;
}
