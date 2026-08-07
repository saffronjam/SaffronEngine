// Materializes one resident micro-field tile's reconstructed blades as real vertices and indices
// for an acceleration-structure build.
//
// The raster executor derives a blade's vertices from a frame-transient candidate. Ray traversal
// has no vertex stage, so a blade reaches a bottom-level structure only as geometry: one workgroup
// per materialized tile re-derives the identical blade set the reconstruction places, bakes the
// same analytic wind bend the raster candidates carry, and writes the shared blade template's ten
// vertices and twenty-four indices per blade into the tile's reserved arena slice.
//
// Indices are relative to the tile's own slice base, which is what the build's `max_vertex` bound
// is stated against. Blades past the tile's reservation are dropped; the slice's cleared tail is
// degenerate triangles the builder discards.
//
// Set 0 is the micro-field pass family's: b2 = the GPU-scene address block. The counters, records,
// and scratch bindings the raster reconstruction writes are not read here.

import global_gpu_data;
import scene_micro_common;

[[vk::binding(2, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

struct Push
{
    float3 eye;
    float maxDistance;
    uint directoryOffset;
    uint directoryCount;
    uint bladesPerTile;
    uint reserved;
    uint64_t vertices;
    uint64_t indices;
    float4 windDirSpeedGust;
    float4 windParams;
    uint windOctaves;
    uint windSeed;
    float windTimeCurrent;
    float windTimePrevious;
    uint64_t windSources;
    uint windSourceCount;
    uint windReserved;
};
[[vk::push_constant]] Push pc;

// `saffron_geometry::Vertex`: position, normal, uv, tangent.
static const uint MICRO_RT_VERTEX_STRIDE = 48u;
// Vertices and indices of the shared blade template (`MICRO_BLADE_VERTEX_COUNT` /
// `MICRO_BLADE_INDEX_COUNT`).
static const uint MICRO_RT_BLADE_VERTICES = 10u;
static const uint MICRO_RT_BLADE_INDICES = 24u;

groupshared uint prefixes[64];
groupshared uint chunkBase;

void writeBladeVertex(uint64_t base, uint vertex, GpuMicroBladeVertex source, float3 tangent)
{
    // Component stores: the interleaved layout puts a three-float field at a 4-byte-aligned
    // offset, which a vector store through a device-address pointer may not assume.
    float* out = (float*)(base + uint64_t(vertex) * MICRO_RT_VERTEX_STRIDE);
    out[0] = source.position.x;
    out[1] = source.position.y;
    out[2] = source.position.z;
    out[3] = source.normal.x;
    out[4] = source.normal.y;
    out[5] = source.normal.z;
    out[6] = source.uv.x;
    out[7] = source.uv.y;
    out[8] = tangent.x;
    out[9] = tangent.y;
    out[10] = tangent.z;
    out[11] = 1.0;
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID, uint3 group: SV_GroupID)
{
    let addresses = sceneAddresses;
    uint tileIndex = group.y;
    if (tileIndex >= pc.directoryCount || pc.vertices == 0 || pc.indices == 0)
    {
        return;
    }
    let entry = microDirectoryEntry(addresses, pc.directoryOffset, tileIndex);
    let tile = microTileHeader(addresses, entry.tileOffset);
    uint texelCount = tile.dims[0] * tile.dims[1] * tile.dims[2];
    uint texelLimit = min(texelCount, SCENE_MICRO_TEXEL_BUDGET);
    uint lane = tid.x;

    uint64_t tileVertices = pc.vertices
        + uint64_t(tileIndex) * uint64_t(pc.bladesPerTile) * uint64_t(MICRO_RT_BLADE_VERTICES)
            * MICRO_RT_VERTEX_STRIDE;
    uint64_t tileIndices = pc.indices
        + uint64_t(tileIndex) * uint64_t(pc.bladesPerTile) * uint64_t(MICRO_RT_BLADE_INDICES) * 4;

    if (lane == 0u)
    {
        chunkBase = 0u;
    }
    GroupMemoryBarrierWithGroupSync();

    for (uint chunkStart = 0u; chunkStart < texelLimit; chunkStart += 64u)
    {
        uint texel = chunkStart + lane;
        MicroTexel context = {};
        if (texel < texelLimit)
        {
            context = microTexel(
                addresses, tile, entry.tileOffset, texel, pc.eye, pc.maxDistance);
        }
        // The frustum verdict is deliberately absent: an off-screen blade still casts and
        // reflects, so every reconstructed blade of a materialized tile is geometry.
        prefixes[lane] = context.blades;
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

        uint slot = prefixes[lane];
        for (uint blade = 0u; blade < context.blades; ++blade)
        {
            if (slot + blade >= pc.bladesPerTile)
            {
                break;
            }
            GpuMicroCandidate candidate;
            microBladePlacement(context, blade, candidate);
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

            uint bladeSlot = slot + blade;
            uint firstVertex = bladeSlot * MICRO_RT_BLADE_VERTICES;
            float3 facing = float3(cos(candidate.yaw), 0.0, sin(candidate.yaw));
            float3 tangent = float3(-facing.z, 0.0, facing.x);
            for (uint vertex = 0u; vertex < MICRO_RT_BLADE_VERTICES; ++vertex)
            {
                writeBladeVertex(
                    tileVertices,
                    firstVertex + vertex,
                    gpuSceneMicroBladeVertex(candidate, vertex, false),
                    tangent);
            }
            // The template's triangle corners: per strip segment, two counter-clockwise triangles
            // over vertex pairs (2i, 2i+1) -> (2i+2, 2i+3), matching
            // `micro_blade_template_indices`.
            uint* out = (uint*)(tileIndices + uint64_t(bladeSlot) * MICRO_RT_BLADE_INDICES * 4);
            for (uint segment = 0u; segment < 4u; ++segment)
            {
                uint corner = firstVertex + segment * 2u;
                uint slotBase = segment * 6u;
                out[slotBase + 0u] = corner;
                out[slotBase + 1u] = corner + 1u;
                out[slotBase + 2u] = corner + 2u;
                out[slotBase + 3u] = corner + 1u;
                out[slotBase + 4u] = corner + 3u;
                out[slotBase + 5u] = corner + 2u;
            }
        }
        GroupMemoryBarrierWithGroupSync();
    }
}
