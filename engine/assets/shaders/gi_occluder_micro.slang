// Aggregate slab occluders for the micro vegetation fields: one thread per resident field-tile
// directory entry appends one SdfInstance covering the volume that tile's reconstructed blades
// occupy, at the tile's own aggregate density.
//
// A grass blade has no CPU instance and no cooked field — it exists only as a device-side
// reconstruction of the tile's density samples — so the instance scatter, which walks the reach
// view's visible list, never sees one. Without this pass a dense field occludes nothing in the
// distance field and grass is absent from every global-illumination march.
//
// The occluder is porous: the tile's authoritative density is what the aggregate carries, not the
// card count the raster reconstruction happens to draw at this cut, so the marches accumulate
// extinction through the slab instead of hitting it.
//
// Set 0 matches the instance scatter's: b2 = the SdfInstance output region for this frame slot,
// b3 = the meta words ([0] written, [1] culled against the reach window, [2] dropped to
// capacity), b4 = the GPU-scene address block. b0/b1 are the reach view's counters and visible
// list, which this pass does not read.

import global_gpu_data;
import mdf_brick;
import scene_micro_common;

[[vk::binding(0, 0)]] StructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<uint> visibleList;
[[vk::binding(2, 0)]] RWStructuredBuffer<SdfInstance> occluders;
[[vk::binding(3, 0)]] RWStructuredBuffer<uint> meta;
[[vk::binding(4, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

struct Push
{
    // The reach window (`gi_occluder_bounds`): a slab outside it cannot affect any march.
    float4 reachMin;
    float4 reachMax;
    // The unit-box brick every slab is backed by: its local bounds and the brick locators the
    // sample needs (`Uploader::upload_unit_box_sdf`).
    float4 localMin;
    float4 localMax;
    uint4 voxelDims;
    uint4 indirDims;
    uint4 atlasBricks;
    // x = bindless SDF slot, y = the brick's max-encode distance (local), z = atlas mip count,
    // w unused.
    float4 field;
    // Element capacity of the occluder output region.
    uint capacity;
    // The resident-tile directory's byte offset within the fields arena, and its entry count.
    uint directoryOffset;
    uint directoryCount;
    uint reserved;
};
[[vk::push_constant]] Push pc;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    if (tid.x >= min(pc.directoryCount, SCENE_MICRO_DIRECTORY_CAPACITY))
    {
        return;
    }
    let entry = microDirectoryEntry(addresses, pc.directoryOffset, tid.x);
    let tile = microTileHeader(addresses, entry.tileOffset);
    if (tile.sampleCount == 0u || entry.predicted == 0u)
    {
        return;
    }

    float3 slabMin;
    float3 slabMax;
    microTileSlab(tile, slabMin, slabMax);
    if (any(slabMax < pc.reachMin.xyz) || any(slabMin > pc.reachMax.xyz))
    {
        InterlockedAdd(meta[1], 1u);
        return;
    }
    float3 extent = max(slabMax - slabMin, float3(1e-4));

    // The tile's mean density: `predicted` is the blade count a fully dense frame reconstructs
    // and one saturated texel contributes SCENE_MICRO_MAX_PER_TEXEL of them, so the ratio is the
    // tile's authored fill. Fully vegetated ground carries MICRO_FIELD_FULL_LEAF_AREA of optical
    // depth, which spread over the slab height is its extinction density.
    float fill = float(entry.predicted)
        / float(tile.sampleCount * SCENE_MICRO_MAX_PER_TEXEL);
    float occupancy = min(
        fill * MICRO_FIELD_FULL_LEAF_AREA
            / (AGGREGATE_EXTINCTION_PER_METER * extent.y),
        AGGREGATE_MAX_OCCUPANCY);

    // The blades draw with the family's slot-0 material, so the slab scatters its proxy albedo.
    let instance = gpuSceneLoadInstance(addresses, entry.instance.index);
    let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);
    let sceneMaterial = gpuScenePrototypeMaterialHandle(
        addresses, prototype.materialRange.first);
    let reference = gpuSceneLoadMaterialReference(addresses, sceneMaterial.index);
    float3 albedo = gpuMaterialProxyAlbedo(
        gpuSceneResidentMaterial(addresses, reference.target));

    // world -> the unit box's local grid: an axis-aligned scale and offset, since a tile's slab
    // is axis-aligned by construction. Rows here; buffer storage is column-major, the same bytes
    // a CPU-written glam matrix carries, so `sampleMdfBrick` reads it unchanged.
    float3 boxExtent = max(pc.localMax.xyz - pc.localMin.xyz, float3(1e-6));
    float3 scale = boxExtent / extent;
    float3 offset = pc.localMin.xyz - slabMin * scale;
    float4x4 worldToLocal;
    worldToLocal[0] = float4(scale.x, 0.0, 0.0, offset.x);
    worldToLocal[1] = float4(0.0, scale.y, 0.0, offset.y);
    worldToLocal[2] = float4(0.0, 0.0, scale.z, offset.z);
    worldToLocal[3] = float4(0.0, 0.0, 0.0, 1.0);

    uint index;
    InterlockedAdd(meta[0], 1u, index);
    if (index >= pc.capacity)
    {
        InterlockedAdd(meta[2], 1u);
        return;
    }
    SdfInstance occluder;
    occluder.worldToLocal = worldToLocal;
    occluder.worldMin = float4(slabMin, albedo.x);
    occluder.worldMax = float4(slabMax, albedo.y);
    occluder.localMin = float4(pc.localMin.xyz, albedo.z);
    occluder.localMax = float4(pc.localMax.xyz, occupancy);
    occluder.params = float4(
        pc.field.x,
        pc.field.y,
        // Local distances scale to world by the mean box-to-slab ratio, the same mean-basis
        // convention the instance scatter uses for a non-uniformly scaled placement.
        (extent.x / boxExtent.x + extent.y / boxExtent.y + extent.z / boxExtent.z) / 3.0,
        pc.field.z);
    occluder.voxelDims = uint4(pc.voxelDims.xyz, 0u);
    occluder.indirDims = uint4(pc.indirDims.xyz, 0u);
    occluder.atlasBricks = uint4(pc.atlasBricks.xyz, 0u);
    occluders[index] = occluder;
}
