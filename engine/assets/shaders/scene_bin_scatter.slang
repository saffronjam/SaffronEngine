// Executor bucketing, pass 3: scatter each record into its bucket's command slice,
// building its VkDrawIndexedIndirectCommand. The pages arena is the executor's index
// buffer for page-resident geometry and the displacement arena's index stream for a
// displaced row; firstInstance carries the record index. A cursor passing the bucket's
// slice end raises the pressure flag and drops the command (never a partial write).

import global_gpu_data;
import scene_bin_common;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<GpuDrawRecord> records;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint> bucketCursors;
[[vk::binding(3, 0)]] RWStructuredBuffer<DrawIndexedIndirectCommand> commands;
[[vk::binding(4, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;
[[vk::binding(5, 0)]] StructuredBuffer<SceneBucketTable> bucketTable;
[[vk::binding(6, 0)]] RWStructuredBuffer<DrawMeshTasksIndirectCommand> meshArgs;

// The mesh executor's dispatch arguments for the same draw, written at the same slot.
void writeMeshArgs(uint slot, uint indexCount)
{
    meshArgs[slot] = meshTaskArgs(indexCount);
}

struct Push
{
    uint recordCapacity;
    uint survivor;
    uint microIndexCount;
    uint microFirstIndex;
};
[[vk::push_constant]] Push pc;

public static const uint SCENE_BUCKET_PRESSURE = 16u;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
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
        return;
    }
    uint slot;
    InterlockedAdd(bucketCursors[bucket], 1u, slot);
    if (slot >= bucketTable[0].ends[bucket])
    {
        InterlockedOr(counters[4], SCENE_BUCKET_PRESSURE);
        return;
    }

    DrawIndexedIndirectCommand command;
    command.instanceCount = 1u;
    command.vertexOffset = 0;
    command.firstInstance = index;
    if (record.representation == GPU_REPRESENTATION_MICRO_BLADE)
    {
        // A blade record draws the shared template block; contentIndex is a
        // candidate slot, never a page handle.
        command.indexCount = pc.microIndexCount;
        command.firstIndex = pc.microFirstIndex;
        commands[slot] = command;
        writeMeshArgs(slot, command.indexCount);
        return;
    }
    if (record.representation == GPU_REPRESENTATION_DISPLACED_MICRO)
    {
        // A displaced record draws its row of the amplification arena: the exact packed
        // index count the emit kernel produced, over the arena's index stream rather than
        // the pages arena. contentIndex is the arena row, never a page handle. The
        // traversal cannot know the count (the arena is sized on device), so the frame's
        // rasterized-triangle counter gains this draw's share here.
        uint indexCount;
        uint firstIndex;
        uint vertexBase;
        gpuSceneDisplacedDraw(addresses, record.contentIndex, indexCount, firstIndex, vertexBase);
        command.indexCount = indexCount;
        command.firstIndex = firstIndex;
        command.vertexOffset = int(vertexBase);
        commands[slot] = command;
        writeMeshArgs(slot, command.indexCount);
        InterlockedAdd(counters[8], indexCount / 3u);
        return;
    }

    GpuHandle pageHandle = GpuHandle(record.contentIndex, 0u);
    let page = gpuSceneResidentPage(addresses, pageHandle);
    let node = gpuScenePageNode(addresses, page);
    uint64_t indexBlob = gpuScenePagePayloadBase(page, node)
        + node.clusterCount * GPU_PAGE_CLUSTER_RECORD_SIZE
        + node.vertexCount * GPU_PAGE_VOXEL_VERTEX_SIZE;
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
    commands[slot] = command;
    writeMeshArgs(slot, command.indexCount);
}
