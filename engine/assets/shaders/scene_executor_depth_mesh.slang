// Mesh-shader executor, depth-only pass: the second executor over the *same* binned records
// the indexed path draws. It reads `scene_bin_scatter`'s command stream as data rather than as
// draw arguments, so the cluster cut is shared rather than independently selected — a
// divergence between the two executors would mean one ignored records the binner emitted.
//
// One workgroup covers MESH_TRIANGLES_PER_GROUP triangles of one draw. `DrawIndex` names which
// draw (the mesh-args stream is parallel to the command stream, and both are indexed from the
// bucket's slice base), and the group id names which block within it. The scatter sized the
// group count from that draw's own index count, so short draws simply dispatch fewer groups.
//
// Vertices are emitted without deduplication: a cluster carries no local vertex table, only a
// flat range into the page index blob, so three vertices go out per triangle. That is what caps
// a group at 62 triangles — 186 output vertices against the 256 limit every supported tier
// reports — not the primitive limit.
//
// The world transform is `transformExecutorVertex`'s arithmetic verbatim, including the wind
// term, because agreement with the indexed executor is vertex-for-vertex or it is nothing.

import global_gpu_data;
import scene_bin_common;

[[vk::binding(0, 0)]] StructuredBuffer<GpuDrawRecord> records;
[[vk::binding(1, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;
[[vk::binding(2, 0)]] StructuredBuffer<DrawIndexedIndirectCommand> commands;

struct Push
{
    float4x4 viewProj;
    // The command-stream slot this bucket's slice starts at; `DrawIndex` is relative to it.
    uint sliceBase;
};
[[vk::push_constant]] Push pc;

struct MeshOut
{
    float4 position : SV_Position;
    nointerpolation uint transition : TEXCOORD0;
};

/// The world position of one pulled index, matching the indexed executor's vertex stage.
// Takes the record *index*, not the record: a by-value `GpuDrawRecord` makes Slang emit a
// natural-layout copy that collides with the std430 layout the BDA pointers below need.
float3 executorWorldPosition(uint recordIndex, uint vertexIndex)
{
    let addresses = sceneAddresses;
    let record = records[recordIndex];
    let instance = gpuSceneLoadInstance(addresses, record.instance.index);
    let geometry = gpuSceneResidentGeometry(addresses, record.geometry);

    bool assembly =
        geometry.parts.count != 0u && record.clusterState != GPU_ASSEMBLY_NO_USE;
    GpuAssemblyUseRecord use = {};
    uint vertexBase = 0u;
    if (assembly)
    {
        use = gpuSceneAssemblyUse(addresses, geometry, geometry.reserved, record.clusterState);
        vertexBase = gpuSceneAssemblyPrototype(addresses, geometry, use.prototype).vertexBase;
    }

    float3 local;
    if (record.representation == GPU_REPRESENTATION_MICRO_BLADE)
    {
        local = gpuSceneMicroBladeVertex(
                    gpuSceneMicroCandidate(addresses, record.contentIndex), vertexIndex, false)
                    .position;
    }
    else if (record.representation == GPU_REPRESENTATION_AGGREGATE_VOXEL)
    {
        GpuHandle pageHandle = GpuHandle(record.contentIndex, 0u);
        let page = gpuSceneResidentPage(addresses, pageHandle);
        let node = gpuScenePageNode(addresses, page);
        local = gpuScenePageVoxelVertex(addresses, page, node, vertexIndex).position;
    }
    else
    {
        float3* position = (float3*)(
            addresses.vertices + geometry.vertices.first
            + uint64_t(vertexBase + vertexIndex) * geometry.vertexStride);
        local = *position;
    }
    if (assembly)
    {
        local = gpuSceneAssemblyTransformPoint(use, local);
    }
    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, false, c0, c1, c2, c3);
    return c0.xyz * local.x + c1.xyz * local.y + c2.xyz * local.z + c3.xyz
        + gpuSceneWindDeform(
            addresses, instance, record.instance.index, local, false, assembly, use,
            record.clusterState);
}

[shader("mesh")]
[outputtopology("triangle")]
[numthreads(MESH_TRIANGLES_PER_GROUP, 1, 1)]
void meshMain(
    uint threadIndex: SV_GroupIndex,
    uint3 groupId: SV_GroupID,
    uint drawIndex: SV_DrawIndex,
    out vertices MeshOut verts[MESH_TRIANGLES_PER_GROUP * 3],
    out indices uint3 tris[MESH_TRIANGLES_PER_GROUP])
{
    let command = commands[pc.sliceBase + drawIndex];

    // Triangles this group owns, clamped to the draw. The scatter derived the group count from
    // this same index count, so the last group is the only partial one.
    let totalTriangles = command.indexCount / 3u;
    let firstTriangle = groupId.x * MESH_TRIANGLES_PER_GROUP;
    uint triangles = 0u;
    if (firstTriangle < totalTriangles)
    {
        triangles = min(MESH_TRIANGLES_PER_GROUP, totalTriangles - firstTriangle);
    }
    SetMeshOutputCounts(triangles * 3u, triangles);
    if (threadIndex >= triangles)
    {
        return;
    }

    let triangle = firstTriangle + threadIndex;
    // `firstIndex` is a u32 offset into the PAGES arena, not the global index arena: the
    // indexed executor binds `gpu_data.pages` as its index buffer and the scatter derives
    // `firstIndex` from a page byte offset. Reading `addresses.indices` here rasterizes nothing.
    uint* indexBlob = (uint*)(sceneAddresses.pageBytes);
    let base = threadIndex * 3u;
    for (uint corner = 0u; corner < 3u; ++corner)
    {
        let vertexIndex = indexBlob[command.firstIndex + triangle * 3u + corner];
        MeshOut output;
        output.position =
            mul(pc.viewProj, float4(executorWorldPosition(command.firstInstance, vertexIndex), 1.0));
        output.transition = records[command.firstInstance].transition;
        verts[base + corner] = output;
    }
    tris[threadIndex] = uint3(base, base + 1u, base + 2u);
}

[shader("fragment")]
void fragmentMain(MeshOut input)
{
    // A record mid representation-crossfade owns only its stochastic share of the pixels — the
    // depth it writes must match the shaded surface exactly.
    if (!gpuTransitionCovered(input.transition, uint2(input.position.xy)))
    {
        discard;
    }
}
