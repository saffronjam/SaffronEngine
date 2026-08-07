// Lit Wireframe overlay: re-draws the scene geometry as edges (PolygonMode::LINE) in a flat
// color over the already-shaded, tonemapped scene. Record-driven like every executor pass:
// SV_VulkanVertexID carries the pulled index value plus the command's `vertexOffset` (the
// D3D-flavoured SV_VertexID subtracts the offset back out, which loses a displaced row's arena
// slice base), SV_VulkanInstanceID the record index; geometry pulls through buffer device
// addresses. The push constant is the camera viewProj. Depth-tested (LESS_OR_EQUAL, no write)
// against the persisted 1× scene depth so hidden edges are occluded.

import global_gpu_data;
import scene_micro_common;

// The frame's semantic record stream (the active view's traversal output).
[[vk::binding(4, 2)]] StructuredBuffer<GpuDrawRecord> executorRecords;

struct Push
{
    float4x4 viewProj;  // world -> clip
};
[[vk::push_constant]] Push pc;

[shader("vertex")]
float4 vertexMain(
    uint vertexIndex : SV_VulkanVertexID, uint recordIndex : SV_VulkanInstanceID) : SV_Position
{
    let addresses = gpuSceneAddresses;
    let record = executorRecords[recordIndex];
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
    else if (record.representation == GPU_REPRESENTATION_DISPLACED_MICRO)
    {
        // The amplification arena carries the displaced position, so the edge follows the
        // surface the scene pass shaded rather than the undisplaced base mesh.
        float3 microNormal;
        float2 microUv;
        gpuSceneDisplacedVertex(addresses, vertexIndex, false, local, microNormal, microUv);
    }
    else if (record.deformation.index != 0xFFFFFFFFu
        && (instance.deformation.index == record.deformation.index))
    {
        // A deformed instance pulls the skinning compute's per-frame output.
        let scene = gpuSceneLoadDeformation(addresses, record.deformation.index);
        let provider = gpuSceneDeformationProvider(addresses, scene.target.index);
        uint firstVertex = gpuSceneDeformationParameter(addresses, provider.firstParameter);
        local = *(float3*)(
            addresses.deformedVertices
            + uint64_t(firstVertex + vertexBase + vertexIndex) * geometry.vertexStride);
    }
    else
    {
        local = *(float3*)(
            addresses.vertices + geometry.vertices.first
            + uint64_t(vertexBase + vertexIndex) * geometry.vertexStride);
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
    return mul(pc.viewProj, float4(world, 1.0));
}

[shader("fragment")]
float4 fragmentMain() : SV_Target
{
    return float4(0.0, 0.85, 0.35, 1.0);  // a bright green edge color
}
