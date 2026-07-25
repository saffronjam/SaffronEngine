// Transparent sort, pass 1: collect the alpha-blended records' sort pairs. The key is
// the flipped-bit view-space depth (larger = nearer), so an ascending radix sort yields
// far-to-near — back-to-front — order. The value is the record index; pairs feed the
// LSD radix passes and the final command reorder.

import global_gpu_data;

public static const uint SCENE_TRANSPARENT_OVERFLOW = 8u;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<GpuDrawRecord> records;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint2> pairs;
[[vk::binding(3, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

struct Push
{
    float4 viewRow2;
    uint recordCapacity;
    uint reserved0;
    uint reserved1;
    uint reserved2;
};
[[vk::push_constant]] Push pc;

// Transparency lives at GPU_PSO_MATERIAL_SHIFT + GPU_MATERIAL_TRANSPARENCY_SHIFT.
bool isTransparent(uint psoBin)
{
    return ((psoBin >> (GPU_PSO_MATERIAL_SHIFT + GPU_MATERIAL_TRANSPARENCY_SHIFT)) & 1u) != 0u;
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    uint total = min(counters[3], pc.recordCapacity);
    if (tid.x >= total)
    {
        return;
    }
    let record = records[tid.x];
    if (!isTransparent(record.psoBin))
    {
        return;
    }
    let instance = gpuSceneLoadInstance(addresses, record.instance.index);
    float4 keyC0, keyC1, keyC2, keyC3;
    gpuSceneInstanceColumns(instance, false, keyC0, keyC1, keyC2, keyC3);
    float3 position = keyC3.xyz;
    // View-space depth magnitude along the camera row; monotone with distance.
    float depth = abs(dot(pc.viewRow2.xyz, position) + pc.viewRow2.w);
    // Flip the float's sign-bit ordering so larger depth sorts LAST ascending; the
    // reorder walks the sorted list in reverse for back-to-front.
    uint bits = asuint(depth);
    uint key = (bits & 0x80000000u) != 0u ? ~bits : bits | 0x80000000u;
    uint slot;
    InterlockedAdd(counters[5], 1u, slot);
    if (slot < pc.recordCapacity)
    {
        pairs[slot] = uint2(key, tid.x);
    }
    else
    {
        InterlockedOr(counters[4], SCENE_TRANSPARENT_OVERFLOW);
    }
}
