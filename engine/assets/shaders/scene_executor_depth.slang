// Indexed-MDI executor, depth-only pass: rasterizes the binned semantic records with
// the pages arena bound as the index buffer. SV_VertexID carries the pulled index value
// (geometry-relative for triangle clusters, page-local for a voxel surface);
// SV_VulkanInstanceID carries the record index the scatter stored in firstInstance.
// Vertices pull through buffer device addresses — no vertex input bindings exist.

import global_gpu_data;

[[vk::binding(0, 0)]] StructuredBuffer<GpuDrawRecord> records;
[[vk::binding(1, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

struct Push
{
    float4x4 viewProj;
};
[[vk::push_constant]] Push pc;

struct VsOut
{
    float4 position : SV_Position;
    // The record's representation-crossfade word (zero outside a transition).
    nointerpolation uint transition : TEXCOORD0;
};

[shader("vertex")]
VsOut vertexMain(uint vertexIndex : SV_VertexID, uint instanceIndex : SV_VulkanInstanceID)
{
    let addresses = sceneAddresses;
    let record = records[instanceIndex];
    let instance = gpuSceneLoadInstance(addresses, record.instance.index);
    let geometry = gpuSceneResidentGeometry(addresses, record.geometry);

    // An assembly record (a plant use) rebases its vertex fetch to the placed
    // prototype's slice and premultiplies the use's family-local transform.
    bool assembly =
        geometry.parts.count != 0u && record.clusterState != GPU_ASSEMBLY_NO_USE;
    GpuAssemblyUseRecord use = {};
    uint vertexBase = 0u;
    if (assembly)
    {
        use = gpuSceneAssemblyUse(addresses, geometry, geometry.reserved, record.clusterState);
        vertexBase =
            gpuSceneAssemblyPrototype(addresses, geometry, use.prototype).vertexBase;
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
        let vertex = gpuScenePageVoxelVertex(addresses, page, node, vertexIndex);
        local = vertex.position;
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
    float3 world = c0.xyz * local.x + c1.xyz * local.y + c2.xyz * local.z + c3.xyz
        + gpuSceneWindDeform(
            addresses, instance, record.instance.index, local, false, assembly, use,
            record.clusterState);
    VsOut output;
    output.position = mul(pc.viewProj, float4(world, 1.0));
    output.transition = record.transition;
    return output;
}

[shader("fragment")]
void fragmentMain(VsOut input)
{
    // A record mid representation-crossfade owns only its stochastic share of the
    // pixels — the depth it writes must match the shaded surface exactly.
    if (!gpuTransitionCovered(input.transition, uint2(input.position.xy)))
    {
        discard;
    }
}
