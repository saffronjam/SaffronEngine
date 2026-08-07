// Executor bucketing, pass 2: seed each bucket's append cursor from its CPU-assigned
// command-slice base.

import scene_bin_common;

[[vk::binding(0, 0)]] StructuredBuffer<SceneBucketTable> bucketTable;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> bucketCursors;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    if (tid.x >= SCENE_EXECUTOR_BUCKET_CAPACITY)
    {
        return;
    }
    bucketCursors[tid.x] = table_base(tid.x);
}

uint table_base(uint bucket)
{
    return bucketTable[0].bases[bucket];
}
