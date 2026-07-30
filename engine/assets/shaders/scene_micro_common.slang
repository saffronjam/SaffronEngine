// Shared micro-field reconstruction vocabulary: the deterministic blade builder and
// per-texel survivor count the count and scatter passes must agree on exactly, plus
// the scratch-buffer layout the count → scan → scatter chain communicates through.
// Candidate placement derives purely from the tile's reconstruction seed and texel
// coordinates — camera travel changes residency, never established placement.

import global_gpu_data;

// One resident micro field tile's header in the fields arena; `sampleCount` u16 density samples
// (padded to four bytes) follow, then `attributeCount` typed channels. `cell` packs the signed
// level-zero owner-cell coordinates as lo/hi word pairs.
public struct GpuFieldTileRecord
{
    public uint cell[6];
    public uint dims[3];
    public uint sampleCount;
    public uint seed[4];
    public uint attributeCount;
    public uint reserved;
};

public static const uint GPU_FIELD_TILE_RECORD_SIZE = 64u;

// One resident-tile directory entry the micro pass dispatches over.
public struct GpuFieldDirectoryEntry
{
    public GpuHandle instance;
    public uint tileOffset;
    public uint predicted;
};

public static const uint GPU_FIELD_DIRECTORY_ENTRY_SIZE = 16u;

// Indices in the shared micro-blade template (eight triangles over the strip).
public static const uint MICRO_BLADE_INDEX_COUNT = 24u;

// One generated micro-blade candidate: the frame-transient placement the executor vertex paths
// reconstruct the blade from (GpuDrawRecord.contentIndex slots).
public struct GpuMicroCandidate
{
    public float3 position;
    public float height;
    public float yaw;
    public float width;
    public uint phenotype;
    public uint reserved;
    public float2 windBend;
    public float2 windBendPrevious;
};

public static const uint GPU_MICRO_CANDIDATE_SIZE = 48u;

public GpuMicroCandidate gpuSceneMicroCandidate(GpuSceneAddressBlock addresses, uint index)
{
    GpuMicroCandidate* candidate = (GpuMicroCandidate*)(
        addresses.candidates + uint64_t(index) * GPU_MICRO_CANDIDATE_SIZE);
    return *candidate;
}

public struct GpuMicroBladeVertex
{
    public float3 position;
    public float3 normal;
    public float2 uv;
};

// One template vertex of a candidate's blade: a four-segment strip tapering toward the tip and
// bowed along its facing, plus the candidate's stored analytic wind bend (tip-weighted, with a
// parabolic tip drop so blades bend rather than stretch). `previous` selects the previous frame's
// bend words for the motion pass. Vertex pairs (2i, 2i+1) sit left/right of the spine at segment
// i; positions are world-space, since blade records ride an identity field instance.
public GpuMicroBladeVertex gpuSceneMicroBladeVertex(
    GpuMicroCandidate candidate, uint vertexIndex, bool previous)
{
    float t = float(vertexIndex / 2u) * 0.25;
    float side = (vertexIndex & 1u) != 0u ? 1.0 : -1.0;
    float3 facing = float3(cos(candidate.yaw), 0.0, sin(candidate.yaw));
    float3 right = float3(-facing.z, 0.0, facing.x);
    float halfWidth = candidate.width * 0.5 * (1.0 - t * 0.85);
    float bow = t * t * candidate.height * 0.3;
    float2 bendWords = previous ? candidate.windBendPrevious : candidate.windBend;
    float3 bend = float3(bendWords.x, 0.0, bendWords.y) * (t * t);
    GpuMicroBladeVertex vertex;
    vertex.position = candidate.position
        + right * (side * halfWidth)
        + float3(0.0, candidate.height * t - dot(bend, bend) / max(2.0 * candidate.height, 0.1), 0.0)
        + facing * bow
        + bend;
    float3 tangent = normalize(float3(0.0, 1.0, 0.0) + facing * (0.6 * t));
    vertex.normal = normalize(cross(right, tangent));
    vertex.uv = float2(side * 0.5 + 0.5, t);
    return vertex;
}

// Texels one directory entry's workgroup covers; a larger tile raises the
// record-pressure flag rather than silently thinning.
public static const uint SCENE_MICRO_TEXEL_BUDGET = 4096u;
// Directory entries the per-tile scratch covers; more raises pressure.
public static const uint SCENE_MICRO_DIRECTORY_CAPACITY = 1024u;
// Blades one fully dense texel reconstructs.
public static const uint SCENE_MICRO_MAX_PER_TEXEL = 4u;
// Scratch words before the per-tile tables: [0] emitted total, [1] record base,
// [2] processed tile count, [3] reserved.
public static const uint SCENE_MICRO_SCRATCH_HEADER = 4u;
// Per-tile base sentinel: the tile did not fit the frame's budgets and is skipped.
public static const uint SCENE_MICRO_BASE_SKIPPED = 0xffffffffu;
public static const uint SCENE_TRAVERSAL_OVERFLOW_RECORDS = 4u;
public static const float BASE_CELL_EDGE_METERS = 64.0;

public struct MicroFieldPush
{
    public float4x4 viewProj;
    public float3 eye;
    public float maxDistance;
    public uint directoryOffset;
    public uint directoryCount;
    public uint recordCapacity;
    public uint candidateCapacity;
    public uint frameBase;
    public uint reserved0;
    public uint reserved1;
    public uint reserved2;
    public float4 windDirSpeedGust;
    public float4 windParams;
    public uint windOctaves;
    public uint windSeed;
    public float windTimeCurrent;
    public float windTimePrevious;
    public uint64_t windSources;
    public uint windSourceCount;
    public uint windReserved;
};

// A small counter-free hash for stable per-texel candidate placement.
public uint microHashCombine(uint seed, uint value)
{
    uint hash = seed ^ (value + 0x9e3779b9u + (seed << 6u) + (seed >> 2u));
    hash ^= hash >> 16u;
    hash *= 0x7feb352du;
    hash ^= hash >> 15u;
    hash *= 0x846ca68bu;
    hash ^= hash >> 16u;
    return hash;
}

public float microHashUnit(uint hash)
{
    return float(hash & 0x00ffffffu) / 16777215.0;
}

public GpuFieldDirectoryEntry microDirectoryEntry(
    GpuSceneAddressBlock addresses, uint directoryOffset, uint index)
{
    GpuFieldDirectoryEntry* entry = (GpuFieldDirectoryEntry*)(
        addresses.fields + directoryOffset
        + uint64_t(index) * GPU_FIELD_DIRECTORY_ENTRY_SIZE);
    return *entry;
}

public GpuFieldTileRecord microTileHeader(GpuSceneAddressBlock addresses, uint tileOffset)
{
    GpuFieldTileRecord* record = (GpuFieldTileRecord*)(addresses.fields + tileOffset);
    return *record;
}

public uint microTileDensity(GpuSceneAddressBlock addresses, uint tileOffset, uint sample)
{
    // Density samples are u16, packed after the 64-byte header.
    uint word = *(uint*)(
        addresses.fields + tileOffset + GPU_FIELD_TILE_RECORD_SIZE
        + uint64_t(sample / 2u) * 4u);
    return (sample & 1u) != 0u ? (word >> 16u) : (word & 0xffffu);
}

// One texel's reconstruction inputs, shared verbatim by count and scatter so both
// passes derive the identical blade set.
public struct MicroTexel
{
    public float3 texelBase;
    public float3 texelSize;
    public uint blades;
    public uint seed;
};

public MicroTexel microTexel(
    GpuSceneAddressBlock addresses,
    GpuFieldTileRecord tile,
    uint tileOffset,
    uint texel,
    float3 eye,
    float maxDistance)
{
    MicroTexel result;
    result.blades = 0u;
    result.texelBase = float3(0.0, 0.0, 0.0);
    result.texelSize = float3(0.0, 0.0, 0.0);
    result.seed = 0u;

    uint density = microTileDensity(addresses, tileOffset, texel);
    if (density == 0u)
    {
        return result;
    }

    // The texel's world footprint: the tile grid spans its owner cell's bounds.
    uint x = texel % tile.dims[0];
    uint y = (texel / tile.dims[0]) % tile.dims[1];
    uint z = texel / (tile.dims[0] * tile.dims[1]);
    result.texelSize = float3(
        BASE_CELL_EDGE_METERS / float(tile.dims[0]),
        BASE_CELL_EDGE_METERS / float(tile.dims[1]),
        BASE_CELL_EDGE_METERS / float(tile.dims[2]));
    float3 cellBase = float3(
        float(int64_t((uint64_t(tile.cell[1]) << 32) | uint64_t(tile.cell[0])))
            * BASE_CELL_EDGE_METERS,
        float(int64_t((uint64_t(tile.cell[3]) << 32) | uint64_t(tile.cell[2])))
            * BASE_CELL_EDGE_METERS,
        float(int64_t((uint64_t(tile.cell[5]) << 32) | uint64_t(tile.cell[4])))
            * BASE_CELL_EDGE_METERS);
    result.texelBase = cellBase + float3(x, y, z) * result.texelSize;

    // Distance gate on the texel center: micro reconstruction is near-field.
    float3 texelCenter = result.texelBase + result.texelSize * 0.5;
    if (length(texelCenter - eye) > maxDistance + length(result.texelSize))
    {
        return result;
    }

    // Blades per texel scale with the quantized density (u16 unit interval).
    result.blades = (density * SCENE_MICRO_MAX_PER_TEXEL + 0xffffu) >> 16u;
    result.seed = tile.seed[0] ^ microHashCombine(tile.seed[1], texel);
    return result;
}

// The deterministic blade for one (texel, blade) pair plus its frustum verdict.
public bool microBlade(
    MicroTexel texel, uint blade, float4x4 viewProj, out GpuMicroCandidate candidate)
{
    uint hash = microHashCombine(texel.seed, blade);
    float jitterX = microHashUnit(microHashCombine(hash, 1u));
    float jitterZ = microHashUnit(microHashCombine(hash, 2u));
    candidate.position = texel.texelBase + float3(jitterX, 0.0, jitterZ) * texel.texelSize;
    candidate.height = 0.25 + 0.35 * microHashUnit(microHashCombine(hash, 3u));
    candidate.yaw = 6.2831853 * microHashUnit(microHashCombine(hash, 4u));
    candidate.width = 0.02 + 0.02 * microHashUnit(microHashCombine(hash, 5u));
    candidate.phenotype = 0u;
    candidate.reserved = 0u;
    // The scatter fills the analytic wind bend for survivors; placement and the
    // survivor count stay bend-independent so count and scatter agree exactly.
    candidate.windBend = float2(0.0, 0.0);
    candidate.windBendPrevious = float2(0.0, 0.0);

    // Frustum test the blade's bounding point (root + height).
    float4 clip = mul(viewProj, float4(candidate.position, 1.0));
    float pad = candidate.height * 2.0;
    return !(clip.w > 0.0
        && (abs(clip.x) > clip.w + pad || abs(clip.y) > clip.w + pad));
}

// Post-cull blade survivors of one texel — the count the scatter must reproduce.
public uint microTexelSurvivors(MicroTexel texel, float4x4 viewProj)
{
    uint survivors = 0u;
    for (uint blade = 0u; blade < texel.blades; ++blade)
    {
        GpuMicroCandidate candidate;
        if (microBlade(texel, blade, viewProj, candidate))
        {
            survivors += 1u;
        }
    }
    return survivors;
}
