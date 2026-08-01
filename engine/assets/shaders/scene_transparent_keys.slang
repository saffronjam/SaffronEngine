// Transparent sort, key stage. Level 0 collects the alpha-blended records' sort pairs;
// the later levels rewrite the sorted pairs' key word in place, once per radix level.
//
// The sort is lexicographic over (view depth, instance slot, page, cluster), least
// significant level first, which is what an LSD radix consumes. Every component is read
// from the record, so the emitted order is a pure function of the frame's record set:
// two records the traversal's atomic append happened to interleave the other way round,
// or a record entering or leaving the stream, cannot move any other record's position.
// Depth alone would leave equal-depth records ordered by that append, which is scheduling
// order and flickers between frames.
//
// The depth word is the flipped-bit view-space depth (larger = nearer), so an ascending
// radix sort ends near-last and the reorder walks it in reverse for back-to-front. The
// pair's value is the record index the reorder resolves the draw from.

import global_gpu_data;

public static const uint SCENE_TRANSPARENT_OVERFLOW = 8u;

// Radix levels, least significant first.
static const uint TRANSPARENT_KEY_CLUSTER = 0u;
static const uint TRANSPARENT_KEY_PAGE = 1u;
static const uint TRANSPARENT_KEY_INSTANCE = 2u;
static const uint TRANSPARENT_KEY_DEPTH = 3u;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] StructuredBuffer<GpuDrawRecord> records;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint2> pairs;
[[vk::binding(3, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

struct Push
{
    float4 viewRow2;
    uint recordCapacity;
    uint keyLevel;
    uint reserved0;
    uint reserved1;
};
[[vk::push_constant]] Push pc;

// Transparency lives at GPU_PSO_MATERIAL_SHIFT + GPU_MATERIAL_TRANSPARENCY_SHIFT.
bool isTransparent(uint psoBin)
{
    return ((psoBin >> (GPU_PSO_MATERIAL_SHIFT + GPU_MATERIAL_TRANSPARENCY_SHIFT)) & 1u) != 0u;
}

uint levelKey(GpuSceneAddressBlock addresses, GpuDrawRecord record, uint level)
{
    if (level == TRANSPARENT_KEY_CLUSTER)
    {
        return record.part;
    }
    if (level == TRANSPARENT_KEY_PAGE)
    {
        return record.contentIndex;
    }
    if (level == TRANSPARENT_KEY_INSTANCE)
    {
        return record.instance.index;
    }
    let instance = gpuSceneLoadInstance(addresses, record.instance.index);
    float4 keyC0, keyC1, keyC2, keyC3;
    gpuSceneInstanceColumns(instance, false, keyC0, keyC1, keyC2, keyC3);
    float3 position = keyC3.xyz;
    // View-space depth magnitude along the camera row; monotone with distance.
    float depth = abs(dot(pc.viewRow2.xyz, position) + pc.viewRow2.w);
    // Flip the float's sign-bit ordering so larger depth sorts LAST ascending.
    uint bits = asuint(depth);
    return (bits & 0x80000000u) != 0u ? ~bits : bits | 0x80000000u;
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    if (pc.keyLevel == TRANSPARENT_KEY_CLUSTER)
    {
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
        uint slot;
        InterlockedAdd(counters[5], 1u, slot);
        if (slot < pc.recordCapacity)
        {
            pairs[slot] = uint2(levelKey(addresses, record, TRANSPARENT_KEY_CLUSTER), tid.x);
        }
        else
        {
            InterlockedOr(counters[4], SCENE_TRANSPARENT_OVERFLOW);
        }
        return;
    }
    uint total = min(counters[5], pc.recordCapacity);
    if (tid.x >= total)
    {
        return;
    }
    let value = pairs[tid.x].y;
    pairs[tid.x].x = levelKey(addresses, records[value], pc.keyLevel);
}
