// Transparent sort, final step: walk the ascending-sorted pairs in reverse (nearest key
// last), gathering each record's indirect command into the transparent command stream
// in back-to-front order. Counter word 5 is the transparent draw count the transparent
// pass's counted indirect draws read. One dispatch per live blend bucket writes that
// bucket's full-length stream at `groupBase`: a pair outside the bucket masks to a
// zero draw, so every bucket's PSO replays the whole sorted order and global
// back-to-front holds across buckets.

import global_gpu_data;
import scene_bin_common;

struct DrawIndexedIndirectCommand
{
    uint indexCount;
    uint instanceCount;
    uint firstIndex;
    int vertexOffset;
    uint firstInstance;
};

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<uint2> pairs;
[[vk::binding(2, 0)]] StructuredBuffer<GpuDrawRecord> records;
[[vk::binding(3, 0)]] RWStructuredBuffer<DrawIndexedIndirectCommand> commands;
[[vk::binding(4, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

struct Push
{
    uint pairCapacity;
    uint groupKey;
    uint groupBase;
    uint reserved0;
};
[[vk::push_constant]] Push pc;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    uint total = min(counters[5], pc.pairCapacity);
    if (tid.x >= total)
    {
        return;
    }
    // Reverse: sorted ascending by flipped-depth key means the farthest is at the end.
    uint2 pair = pairs[total - 1u - tid.x];
    let record = records[pair.y];
    if (sceneBucketKey(record.reserved, record.psoBin) != pc.groupKey)
    {
        // Another bucket's draw: this group's stream masks it to a zero draw so the
        // slot still holds its back-to-front position.
        DrawIndexedIndirectCommand masked;
        masked.indexCount = 0u;
        masked.instanceCount = 0u;
        masked.firstIndex = 0u;
        masked.vertexOffset = 0;
        masked.firstInstance = pair.y;
        commands[pc.groupBase + tid.x] = masked;
        return;
    }

    GpuHandle pageHandle = GpuHandle(record.contentIndex, 0u);
    let page = gpuSceneResidentPage(addresses, pageHandle);
    let node = gpuScenePageNode(addresses, page);
    uint64_t indexBlob = gpuScenePagePayloadBase(page, node)
        + node.clusterCount * GPU_PAGE_CLUSTER_RECORD_SIZE
        + node.vertexCount * GPU_PAGE_VOXEL_VERTEX_SIZE;

    DrawIndexedIndirectCommand command;
    command.instanceCount = 1u;
    command.vertexOffset = 0;
    command.firstInstance = pair.y;
    if (node.representation == GPU_REPRESENTATION_AGGREGATE_VOXEL)
    {
        command.indexCount = node.indexCount;
        command.firstIndex = uint(indexBlob / 4u);
    }
    else
    {
        let cluster = gpuScenePageCluster(addresses, page, node, record.part);
        command.indexCount = cluster.indexCount;
        command.firstIndex = uint(indexBlob / 4u) + cluster.firstIndex;
    }
    commands[pc.groupBase + tid.x] = command;
}
