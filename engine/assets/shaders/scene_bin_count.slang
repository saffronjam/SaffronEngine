// Executor bucketing, pass 1: count the semantic records per draw bucket. A record's
// bucket comes from the CPU-enumerated (shaderIndex, psoBin) lookup; a miss raises the
// pressure flag (counter word 4).

import global_gpu_data;
import scene_bin_common;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<GpuDrawRecord> records;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint> bucketCounts;
[[vk::binding(3, 0)]] StructuredBuffer<SceneBucketTable> bucketTable;

struct Push
{
    uint recordCapacity;
    uint survivor;
    uint reserved1;
    uint reserved2;
};
[[vk::push_constant]] Push pc;

public static const uint SCENE_BUCKET_PRESSURE = 16u;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    // A survivor pass processes only records appended after the provisional
    // snapshot (counter word 7).
    uint base = pc.survivor != 0u ? counters[7] : 0u;
    uint index = base + tid.x;
    uint total = min(counters[3], pc.recordCapacity);
    if (index >= total)
    {
        return;
    }
    let record = records[index];
    uint bucket = sceneBucketFor(bucketTable, sceneBucketKey(record.reserved, record.psoBin));
    if (bucket == 0xFFFFFFFFu)
    {
        InterlockedOr(counters[4], SCENE_BUCKET_PRESSURE);
        return;
    }
    uint previous;
    InterlockedAdd(bucketCounts[bucket], 1u, previous);
    // Counter word 18: buckets that received their first record. Counting the transition rather
    // than scanning the table afterwards keeps this on the pass that already touches every record,
    // and the answer is the number of indirect draws the executor will issue.
    if (previous == 0u)
    {
        InterlockedAdd(counters[18], 1u);
    }
}
