// Micro-field scatter: one workgroup per resident-tile directory entry re-derives the
// identical blade set the count pass measured and writes each survivor's candidate and
// semantic GpuDrawRecord at its exact scanned slot — no atomics, so the record stream
// order is bitwise stable frame to frame. Tiles the scan skipped emit nothing.

import global_gpu_data;
import scene_micro_common;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] RWStructuredBuffer<GpuDrawRecord> records;
[[vk::binding(2, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;
[[vk::binding(3, 0)]] RWStructuredBuffer<uint> scratch;

[[vk::push_constant]] MicroFieldPush pc;

groupshared uint prefixes[64];
groupshared uint chunkBase;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID, uint3 group : SV_GroupID)
{
    let addresses = sceneAddresses;
    uint tileIndex = group.y;
    if (tileIndex >= min(pc.directoryCount, SCENE_MICRO_DIRECTORY_CAPACITY))
    {
        return;
    }
    uint tileBase =
        scratch[SCENE_MICRO_SCRATCH_HEADER + SCENE_MICRO_DIRECTORY_CAPACITY + tileIndex];
    if (tileBase == SCENE_MICRO_BASE_SKIPPED)
    {
        return;
    }
    let entry = microDirectoryEntry(addresses, pc.directoryOffset, tileIndex);
    let tile = microTileHeader(addresses, entry.tileOffset);
    uint texelCount = tile.dims[0] * tile.dims[1] * tile.dims[2];
    uint texelLimit = min(texelCount, SCENE_MICRO_TEXEL_BUDGET);
    uint lane = tid.x;
    uint recordBase = scratch[1];

    let instance = gpuSceneLoadInstance(addresses, entry.instance.index);
    let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);
    let sceneMaterial = gpuScenePrototypeMaterialHandle(
        addresses, prototype.materialRange.first);
    let materialReference = gpuSceneLoadMaterialReference(addresses, sceneMaterial.index);
    let material = gpuSceneResidentMaterial(addresses, materialReference.target);

    if (lane == 0u)
    {
        chunkBase = 0u;
    }
    GroupMemoryBarrierWithGroupSync();

    for (uint chunkStart = 0u; chunkStart < texelLimit; chunkStart += 64u)
    {
        uint texel = chunkStart + lane;
        MicroTexel context = {};
        uint survivors = 0u;
        if (texel < texelLimit)
        {
            context = microTexel(
                addresses, tile, entry.tileOffset, texel, pc.eye, pc.maxDistance);
            survivors = microTexelSurvivors(context, pc.viewProj);
        }
        prefixes[lane] = survivors;
        GroupMemoryBarrierWithGroupSync();
        if (lane == 0u)
        {
            uint running = chunkBase;
            for (uint index = 0u; index < 64u; ++index)
            {
                uint count = prefixes[index];
                prefixes[index] = running;
                running += count;
            }
            chunkBase = running;
        }
        GroupMemoryBarrierWithGroupSync();

        uint slot = tileBase + prefixes[lane];
        uint blades = texel < texelLimit && survivors > 0u ? context.blades : 0u;
        for (uint blade = 0u; blade < blades; ++blade)
        {
            GpuMicroCandidate candidate;
            if (!microBlade(context, blade, pc.viewProj, candidate))
            {
                continue;
            }
            // The survivor's analytic wind bend, baked once per frame; every raster pass
            // reads the stored words.
            microBladeBakeBend(
                addresses.interactionField,
                pc.windDirSpeedGust,
                pc.windParams,
                pc.windOctaves,
                pc.windSeed,
                pc.windTimeCurrent,
                pc.windTimePrevious,
                pc.windSources,
                pc.windSourceCount,
                candidate);
            uint candidateIndex = pc.frameBase + slot;
            GpuMicroCandidate* out = (GpuMicroCandidate*)(
                addresses.candidates
                + uint64_t(candidateIndex) * GPU_MICRO_CANDIDATE_SIZE);
            *out = candidate;

            GpuDrawRecord record;
            record.geometry = prototype.geometry;
            record.material = sceneMaterial;
            record.instance = GpuHandle(entry.instance.index, instance.sourceGeneration);
            record.deformation = GpuHandle(0xffffffffu, 0u);
            record.contentIndex = candidateIndex;
            record.part = 0u;
            record.representation = GPU_REPRESENTATION_MICRO_BLADE;
            record.sourceGeneration = instance.sourceGeneration;
            record.psoBin = composeGpuPsoBin(
                GPU_REPRESENTATION_TRIANGLE_CLUSTER,
                material.materialClass,
                GPU_DEFORMATION_RIGID,
                0u);
            record.transition = 0u;
            record.clusterState = GPU_ASSEMBLY_NO_USE;
            record.reserved = material.shaderIndex;
            records[recordBase + slot] = record;
            slot += 1u;
        }
        GroupMemoryBarrierWithGroupSync();
    }
}
