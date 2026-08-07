// Micro-field scan: one thread walks the per-tile survivor counts in directory order,
// assigns each tile an exact exclusive base under the frame's candidate and record
// budgets, and reserves the emitted range on the shared counters once. A tile that
// does not fit — and every tile after it — is skipped whole with the pressure flag
// raised; density is never thinned silently and no partial tile ever emits.

import global_gpu_data;
import scene_micro_common;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] RWStructuredBuffer<GpuDrawRecord> records;
[[vk::binding(2, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;
[[vk::binding(3, 0)]] RWStructuredBuffer<uint> scratch;

[[vk::push_constant]] MicroFieldPush pc;

[shader("compute")]
[numthreads(1, 1, 1)]
void computeMain()
{
    uint tileCount = min(pc.directoryCount, SCENE_MICRO_DIRECTORY_CAPACITY);
    // The traversal's record total is final at this graph point; micro appends after it.
    uint recordBase = counters[3];
    uint recordRoom = pc.recordCapacity > recordBase ? pc.recordCapacity - recordBase : 0u;
    uint budget = min(pc.candidateCapacity, recordRoom);

    uint running = 0u;
    bool skipping = false;
    for (uint tile = 0u; tile < tileCount; ++tile)
    {
        uint count = scratch[SCENE_MICRO_SCRATCH_HEADER + tile];
        if (skipping || running + count > budget)
        {
            // Strict prefix semantics: the first tile that does not fit ends the
            // frame's emission so slots stay exact and deterministic.
            if (!skipping && count > 0u)
            {
                InterlockedOr(counters[4], SCENE_TRAVERSAL_OVERFLOW_RECORDS);
            }
            skipping = true;
            scratch[SCENE_MICRO_SCRATCH_HEADER + SCENE_MICRO_DIRECTORY_CAPACITY + tile] =
                SCENE_MICRO_BASE_SKIPPED;
            continue;
        }
        scratch[SCENE_MICRO_SCRATCH_HEADER + SCENE_MICRO_DIRECTORY_CAPACITY + tile] = running;
        running += count;
    }

    scratch[0] = running;
    scratch[1] = recordBase;
    scratch[2] = tileCount;
    counters[3] = recordBase + running;
    counters[9] += running;
    counters[8] += running * (MICRO_BLADE_INDEX_COUNT / 3u);
}
