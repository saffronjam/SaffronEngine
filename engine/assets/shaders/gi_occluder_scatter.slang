// GI occluder scatter: turns the reach view's visible list into this frame's SDF
// occluder instances, on device. One thread per reach-visible instance walks the
// prototype's baked-field range and appends one occluder per field whose world AABB
// meets the reach window, so every field stays as tight as a CPU gather would make it.
// The consumers — the GDF cull/composite and the DDGI near-field march — read the count
// from the meta words, since a GPU-produced count cannot ride a push constant.
//
// Set 0: b0 = the reach view's counters (word 0 is its visible count), b1 = its
// visible slot list, b2 = the SdfInstance output region for this frame slot, b3 = the
// meta words ([0] written, [1] culled against the reach window, [2] dropped to
// capacity), b4 = the GPU-scene address block.

import global_gpu_data;
import mdf_brick;

[[vk::binding(0, 0)]] StructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<uint> visibleList;
[[vk::binding(2, 0)]] RWStructuredBuffer<SdfInstance> occluders;
[[vk::binding(3, 0)]] RWStructuredBuffer<uint> meta;
[[vk::binding(4, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

struct Push
{
    // The reach window (`gi_occluder_bounds`): a field outside it cannot affect any
    // march, so it is culled per FIELD — an instance straddling the window edge keeps
    // only the fields inside it.
    float4 reachMin;
    float4 reachMax;
    // Element capacity of the occluder output region.
    uint capacity;
    // Element capacity of the visible list.
    uint listCapacity;
    uint2 reserved;
};
[[vk::push_constant]] Push pc;

// Slot 0's material through the instance's sparse overrides — the same resolution the
// traversal draws with, so the occluder's proxy albedo and occupancy are the drawn
// material's.
GpuMaterialTableRecord resolveOccluderMaterial(
    GpuSceneAddressBlock addresses,
    GpuSceneInstanceGpuRecord instance,
    GpuScenePrototypeGpuRecord prototype)
{
    GpuHandle sceneMaterial = gpuScenePrototypeMaterialHandle(
        addresses, prototype.materialRange.first);
    for (uint element = 0u; element < instance.materialOverrides.count; ++element)
    {
        let override = gpuSceneOverrideElement(
            addresses, instance.materialOverrides.first + element);
        if (override.slot == 0u)
        {
            sceneMaterial = override.material;
            break;
        }
    }
    let reference = gpuSceneLoadMaterialReference(addresses, sceneMaterial.index);
    return gpuSceneResidentMaterial(addresses, reference.target);
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    uint visible = min(counters[0], pc.listCapacity);
    if (tid.x >= visible)
    {
        return;
    }
    uint slot = visibleList[tid.x];
    let instance = gpuSceneLoadInstance(addresses, slot);
    let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);
    if (prototype.sdfRange.count == 0u)
    {
        return;
    }

    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, false, c0, c1, c2, c3);
    // The fields store distances in local units; the marches compare in world space.
    // The mean basis-column length converts one to the other, shared by every field of
    // this instance (they share its local frame).
    float worldScale = (length(c0.xyz) + length(c1.xyz) + length(c2.xyz)) / 3.0;

    // world→local of the affine instance transform, via the 3×3 adjugate. Built as
    // matrix rows; buffer storage is column-major, the same bytes a CPU-written glam
    // matrix carried, so `sampleMdfBrick` reads it unchanged.
    float3 r0 = cross(c1.xyz, c2.xyz);
    float3 r1 = cross(c2.xyz, c0.xyz);
    float3 r2 = cross(c0.xyz, c1.xyz);
    float det = dot(c0.xyz, r0);
    float invDet = abs(det) > 1e-12 ? 1.0 / det : 0.0;
    r0 *= invDet;
    r1 *= invDet;
    r2 *= invDet;
    float3 tInv = -float3(dot(r0, c3.xyz), dot(r1, c3.xyz), dot(r2, c3.xyz));
    float4x4 worldToLocal;
    worldToLocal[0] = float4(r0, tInv.x);
    worldToLocal[1] = float4(r1, tInv.y);
    worldToLocal[2] = float4(r2, tInv.z);
    worldToLocal[3] = float4(0.0, 0.0, 0.0, 1.0);

    let material = resolveOccluderMaterial(addresses, instance, prototype);
    float3 materialAlbedo = gpuMaterialProxyAlbedo(material);

    for (uint field = 0u; field < prototype.sdfRange.count; ++field)
    {
        let sceneHandle =
            gpuScenePrototypeSdfHandle(addresses, prototype.sdfRange.first + field);
        let reference = gpuSceneLoadSdfReference(addresses, sceneHandle.index);
        let record = gpuSceneResidentSdf(addresses, reference.target);

        // The field's own world AABB — tight per field, so a straddling instance keeps
        // only the fields any march can reach.
        float3 worldMin = float3(1e30);
        float3 worldMax = float3(-1e30);
        for (uint corner = 0u; corner < 8u; ++corner)
        {
            float3 local = float3(
                (corner & 1u) != 0u ? record.localMax.x : record.localMin.x,
                (corner & 2u) != 0u ? record.localMax.y : record.localMin.y,
                (corner & 4u) != 0u ? record.localMax.z : record.localMin.z);
            float3 world = c0.xyz * local.x + c1.xyz * local.y + c2.xyz * local.z + c3.xyz;
            worldMin = min(worldMin, world);
            worldMax = max(worldMax, world);
        }
        if (any(worldMax < pc.reachMin.xyz) || any(worldMin > pc.reachMax.xyz))
        {
            InterlockedAdd(meta[1], 1u);
            continue;
        }

        // A cooked field carries its own CALIBRATED occupancy and albedo (a plant's
        // slot-0 material is its trunk, and a canopy is not a solid); zero means the
        // field carries none and the drawn material's values stand — every triangle
        // bake.
        // The field states its own occupancy: a triangle bake writes SOLID (walls
        // occlude), a cooked plant field writes its calibrated aggregate. The drawn
        // material never stands in — slot 0 of a placed plant is its trunk.
        float occupancy = record.localMax.w;
        uint packedAlbedo = record.atlasBricks.w;
        float3 albedo = packedAlbedo != 0u
            ? float3(
                  float(packedAlbedo & 0xffu),
                  float((packedAlbedo >> 8u) & 0xffu),
                  float((packedAlbedo >> 16u) & 0xffu))
                / 255.0
            : materialAlbedo;

        uint index;
        InterlockedAdd(meta[0], 1u, index);
        if (index >= pc.capacity)
        {
            InterlockedAdd(meta[2], 1u);
            continue;
        }
        SdfInstance occluder;
        occluder.worldToLocal = worldToLocal;
        occluder.worldMin = float4(worldMin, albedo.x);
        occluder.worldMax = float4(worldMax, albedo.y);
        occluder.localMin = float4(record.localMin.xyz, albedo.z);
        occluder.localMax = float4(record.localMax.xyz, occupancy);
        occluder.params = float4(
            float(record.voxelDims.w),
            record.localMin.w,
            worldScale,
            float(record.indirectionDims.w));
        occluder.voxelDims = uint4(record.voxelDims.xyz, 0u);
        occluder.indirDims = uint4(record.indirectionDims.xyz, 0u);
        occluder.atlasBricks = uint4(record.atlasBricks.xyz, 0u);
        occluders[index] = occluder;
    }
}
