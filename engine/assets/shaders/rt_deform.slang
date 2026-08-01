// Materializes one deforming instance's world-space geometry for an acceleration-structure
// build: per vertex, the assembly use's family-local transform, the instance world transform,
// and the wind prepass record's displacement — the same three terms `vertexMainExecutor`
// applies, read from the same record, so a ray and a raster pixel see one pose.
//
// Ray traversal has no vertex stage, so a swaying plant reaches a bottom-level structure only
// as vertices. One dispatch per materialized use; the destination slice is the shared deformed
// arena the skinning refit path already builds from.
import global_gpu_data;

// The mesh's own static vertex stream and the destination slice, both by device address, plus
// the use transform and the identity the wind record is keyed by. Positions come from
// `saffron_geometry::Vertex` (48 bytes: position, normal, uv, tangent) and the destination
// carries the same layout, since every consumer of the deformed arena reads it as one.
struct RtDeformPush
{
    float4 useRow0;
    float4 useRow1;
    float4 useRow2;
    uint64_t source;
    uint64_t dest;
    // The first vertex of the mesh's stream this slice mirrors — an assembly prototype's run
    // base, zero for a whole mesh.
    uint firstVertex;
    uint vertexCount;
    uint instanceSlot;
    // The use's ordinal in the family's table, which is what gives its branch mode a stable
    // phase, and its structural semantic.
    uint useIndex;
    uint semantic;
    // Nonzero when the use transform applies (a family), zero for a plain mesh.
    uint assembly;
    uint2 reserved;
};

[[vk::push_constant]] RtDeformPush pc;

[[vk::binding(5, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

static const uint RT_DEFORM_VERTEX_STRIDE = 48u;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    uint vertex = tid.x;
    if (vertex >= pc.vertexCount || pc.source == 0 || pc.dest == 0)
    {
        return;
    }
    if (pc.instanceSlot >= addresses.instanceCapacity)
    {
        return;
    }
    let instance = gpuSceneLoadInstance(addresses, pc.instanceSlot);

    uint64_t src = pc.source + uint64_t(pc.firstVertex + vertex) * RT_DEFORM_VERTEX_STRIDE;
    uint64_t dst = pc.dest + uint64_t(vertex) * RT_DEFORM_VERTEX_STRIDE;
    float* base = (float*)src;
    float3 local = float3(base[0], base[1], base[2]);
    float3 localNormal = float3(base[3], base[4], base[5]);
    float2 uv = float2(base[6], base[7]);
    float4 tangent = float4(base[8], base[9], base[10], base[11]);

    GpuAssemblyUseRecord use;
    use.row0 = pc.useRow0;
    use.row1 = pc.useRow1;
    use.row2 = pc.useRow2;
    use.prototype = 0u;
    use.reserved0 = pc.semantic;
    use.reserved1 = 0u;
    use.reserved2 = 0u;
    bool assembly = pc.assembly != 0u;
    if (assembly)
    {
        local = gpuSceneAssemblyTransformPoint(use, local);
        localNormal = normalize(gpuSceneAssemblyTransformVector(use, localNormal));
    }

    float3 windOffset = gpuSceneWindDeform(
        addresses, instance, pc.instanceSlot, local, false, assembly, use, pc.useIndex);

    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, false, c0, c1, c2, c3);
    float3 world = c0.xyz * local.x + c1.xyz * local.y + c2.xyz * local.z + c3.xyz + windOffset;
    // Cofactor normals, so a non-uniformly scaled placement stays correct — the same transform
    // `transformExecutorVertex` applies.
    float3 n0 = cross(c1.xyz, c2.xyz);
    float3 n1 = cross(c2.xyz, c0.xyz);
    float3 n2 = cross(c0.xyz, c1.xyz);
    float3 worldNormal =
        normalize(n0 * localNormal.x + n1 * localNormal.y + n2 * localNormal.z);
    float3 worldTangent = normalize(
        c0.xyz * tangent.x + c1.xyz * tangent.y + c2.xyz * tangent.z);

    // Component stores: the interleaved layout puts a three-float field at a 4-byte-aligned
    // offset, which a vector store through a device-address pointer may not assume.
    float* out = (float*)dst;
    out[0] = world.x;
    out[1] = world.y;
    out[2] = world.z;
    out[3] = worldNormal.x;
    out[4] = worldNormal.y;
    out[5] = worldNormal.z;
    out[6] = uv.x;
    out[7] = uv.y;
    out[8] = worldTangent.x;
    out[9] = worldTangent.y;
    out[10] = worldTangent.z;
    out[11] = tangent.w;
}
