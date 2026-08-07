// Micro-field count: one workgroup per resident-tile directory entry strides the
// tile's texels and reduces the post-cull blade survivor count into the per-tile
// scratch table. The scan pass turns the counts into exact output bases; nothing is
// emitted here and no global atomics order the stream.

import global_gpu_data;
import scene_micro_common;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] RWStructuredBuffer<GpuDrawRecord> records;
[[vk::binding(2, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;
[[vk::binding(3, 0)]] RWStructuredBuffer<uint> scratch;

[[vk::push_constant]] MicroFieldPush pc;

groupshared uint partials[64];

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID, uint3 group : SV_GroupID)
{
    let addresses = sceneAddresses;
    uint tileIndex = group.y;
    if (tileIndex >= min(pc.directoryCount, SCENE_MICRO_DIRECTORY_CAPACITY))
    {
        return;
    }
    let entry = microDirectoryEntry(addresses, pc.directoryOffset, tileIndex);
    let tile = microTileHeader(addresses, entry.tileOffset);
    uint texelCount = tile.dims[0] * tile.dims[1] * tile.dims[2];
    uint texelLimit = min(texelCount, SCENE_MICRO_TEXEL_BUDGET);
    uint lane = tid.x;
    if (lane == 0u)
    {
        if (texelCount > SCENE_MICRO_TEXEL_BUDGET)
        {
            // The tile exceeds the texel budget: raise pressure, never thin silently.
            InterlockedOr(counters[4], SCENE_TRAVERSAL_OVERFLOW_RECORDS);
        }
        if (pc.directoryCount > SCENE_MICRO_DIRECTORY_CAPACITY)
        {
            InterlockedOr(counters[4], SCENE_TRAVERSAL_OVERFLOW_RECORDS);
        }
    }

    uint total = 0u;
    for (uint texel = lane; texel < texelLimit; texel += 64u)
    {
        let context = microTexel(
            addresses, tile, entry.tileOffset, texel, pc.eye, pc.maxDistance);
        total += microTexelSurvivors(context, pc.viewProj);
    }
    partials[lane] = total;
    GroupMemoryBarrierWithGroupSync();
    if (lane == 0u)
    {
        uint sum = 0u;
        for (uint index = 0u; index < 64u; ++index)
        {
            sum += partials[index];
        }
        scratch[SCENE_MICRO_SCRATCH_HEADER + tileIndex] = sum;
    }
}
