// Byte-locked global GPU tables, draw records, and fixed PSO vocabulary.

import coverage;

public static const uint GLOBAL_GPU_DATA_ABI_VERSION = 2u;

public static const uint GPU_PSO_REPRESENTATION_SHIFT = 0u;
public static const uint GPU_PSO_MATERIAL_SHIFT = 2u;
public static const uint GPU_PSO_COVERAGE_SHIFT = 2u;
public static const uint GPU_PSO_SIDEDNESS_SHIFT = 4u;
public static const uint GPU_PSO_SURFACE_MODEL_SHIFT = 5u;
public static const uint GPU_PSO_TRANSPARENCY_SHIFT = 6u;
public static const uint GPU_PSO_DEFORMATION_SHIFT = 8u;
public static const uint GPU_PSO_PASS_SHIFT = 9u;

public static const uint GPU_MATERIAL_COVERAGE_SHIFT = 0u;
public static const uint GPU_MATERIAL_SIDEDNESS_SHIFT = 2u;
public static const uint GPU_MATERIAL_SURFACE_MODEL_SHIFT = 3u;
public static const uint GPU_MATERIAL_TRANSPARENCY_SHIFT = 4u;
public static const uint GPU_MATERIAL_UNLIT_SHIFT = 5u;

public static const uint GPU_REPRESENTATION_TRIANGLE_CLUSTER = 0u;
public static const uint GPU_REPRESENTATION_AGGREGATE_VOXEL = 1u;
public static const uint GPU_REPRESENTATION_MICRO_BLADE = 2u;
public static const uint GPU_REPRESENTATION_DISPLACED_MICRO = 3u;
public static const uint GPU_DEFORMATION_RIGID = 0u;
public static const uint GPU_DEFORMATION_COMMON_OUTPUT = 1u;
public static const uint GPU_PASS_WIRE_DEBUG = 9u;

public uint composeGpuMaterialClass(
    uint coverage, uint sidedness, uint surfaceModel, uint transparency, uint unlit)
{
    return (coverage << GPU_MATERIAL_COVERAGE_SHIFT)
        | (sidedness << GPU_MATERIAL_SIDEDNESS_SHIFT)
        | (surfaceModel << GPU_MATERIAL_SURFACE_MODEL_SHIFT)
        | (transparency << GPU_MATERIAL_TRANSPARENCY_SHIFT)
        | (unlit << GPU_MATERIAL_UNLIT_SHIFT);
}

public uint composeGpuPsoBin(uint representation, uint materialClass, uint deformation, uint passClass)
{
    return (representation << GPU_PSO_REPRESENTATION_SHIFT)
        | (materialClass << GPU_PSO_MATERIAL_SHIFT)
        | (deformation << GPU_PSO_DEFORMATION_SHIFT)
        | (passClass << GPU_PSO_PASS_SHIFT);
}

public struct GpuHandle
{
    public uint index;
    public uint generation;
};

public struct GpuArenaRange
{
    public uint first;
    public uint count;
};

public struct GpuTableSlotHeader
{
    public uint generation;
    public uint occupied;
    public uint2 reserved;
};

public bool gpuHandleMatches(GpuHandle handle, GpuTableSlotHeader header)
{
    return header.occupied != 0u && header.generation == handle.generation;
}

public struct GpuPrototypeRecord
{
    public GpuHandle geometry;
    public GpuArenaRange materialRange;
    public GpuHandle skeleton;
    public GpuHandle rootPage;
    public float4 bounds;
    public uint flags;
    public uint3 reserved;
};

public struct GpuGeometryRecord
{
    public GpuArenaRange vertices;
    public GpuArenaRange indices;
    public GpuArenaRange clusters;
    public GpuArenaRange parts;
    public GpuArenaRange voxels;
    public GpuArenaRange submeshes;
    public uint flags;
    public uint vertexStride;
    public uint indexStride;
    public uint reserved;
};

public struct GpuSubmeshRecord
{
    public uint firstIndex;
    public uint indexCount;
    public uint materialSlot;
    public uint reserved;
};

// One assembly prototype's entry in a geometry's parts range: its slice of the use records and
// its base vertex within the geometry's flattened vertex range. The prototype count rides
// GpuGeometryRecord.reserved.
public struct GpuAssemblyPrototypeRecord
{
    public uint firstUse;
    public uint useCount;
    public uint vertexBase;
    public uint reserved;
};

// One assembly use: the prototype it places and rows 0-2 of its row-major family-local
// transform (the implicit last row is [0, 0, 0, 1]).
//
// The reserved words are three scalars rather than a `uint3`: this record is read through a
// buffer device address, and std430 requires a three-component vector to start on a 16-byte
// boundary. The block begins at offset 52, so a `uint3` there fails spirv-val in every module
// that reaches this path. The bytes and the 64-byte size are identical either way.
public struct GpuAssemblyUseRecord
{
    public float4 row0;
    public float4 row1;
    public float4 row2;
    public uint prototype;
    public uint reserved0;
    public uint reserved1;
    public uint reserved2;
};

public static const uint GPU_ASSEMBLY_PROTOTYPE_RECORD_SIZE = 16u;
public static const uint GPU_ASSEMBLY_USE_RECORD_SIZE = 64u;
// GpuDrawRecord.clusterState: the record draws outside any assembly.
public static const uint GPU_ASSEMBLY_NO_USE = 0xFFFFFFFFu;

// GpuDrawRecord.transition — the representation crossfade word. Zero is settled.
// Active: phase (1..=GPU_TRANSITION_FRAMES) | the outgoing bit | the active bit |
// the 16-bit flip id (a stable hash of the flip node's instance slot and page, shared
// by both sides so their stochastic coverage partitions every pixel exactly).
public static const uint GPU_TRANSITION_FRAMES = 16u;
public static const uint GPU_TRANSITION_PHASE_MASK = 0x3fu;
public static const uint GPU_TRANSITION_OUTGOING = 0x40u;
public static const uint GPU_TRANSITION_ACTIVE = 0x80u;

public uint gpuTransitionPack(uint phase, bool outgoing, uint flipId)
{
    return (phase & GPU_TRANSITION_PHASE_MASK)
        | (outgoing ? GPU_TRANSITION_OUTGOING : 0u)
        | GPU_TRANSITION_ACTIVE
        | (flipId << 16u);
}

// The temporally stable stochastic coverage test: a frame-free per-pixel dither keyed on the flip
// id decides which representation owns the pixel at this phase. The incoming side keeps pixels
// below the phase threshold and the outgoing side the complement, so the two together cover every
// pixel exactly once — the crossfade never holes and never double-draws.
public bool gpuTransitionCovered(uint transition, uint2 pixel)
{
    if (transition == 0u)
    {
        return true;
    }
    uint phase = transition & GPU_TRANSITION_PHASE_MASK;
    uint flip = transition >> 16u;
    uint hash = (pixel.x * 0x9e3779b9u) ^ (pixel.y * 0x85ebca6bu) ^ (flip * 0xc2b2ae35u);
    hash ^= hash >> 15u;
    hash *= 0x2c1b3c6du;
    hash ^= hash >> 12u;
    float dither = float(hash & 0xffffu) / 65536.0;
    float threshold = float(phase) / float(GPU_TRANSITION_FRAMES);
    bool incoming = (transition & GPU_TRANSITION_OUTGOING) == 0u;
    return incoming ? dither < threshold : dither >= threshold;
}

// The parts range's fixed header. It is followed by the prototype table, the use records, then
// the per-combination active-use mask words.
public struct GpuAssemblyHeaderRecord
{
    public uint prototypeCount;
    public uint useCount;
    public uint maskWords;
    public uint combinationCount;
};

public static const uint GPU_ASSEMBLY_HEADER_SIZE = 16u;

public GpuAssemblyHeaderRecord gpuSceneAssemblyHeader(
    GpuSceneAddressBlock addresses, GpuGeometryRecord geometry)
{
    GpuAssemblyHeaderRecord* record =
        (GpuAssemblyHeaderRecord*)(addresses.parts + geometry.parts.first);
    return *record;
}

public GpuAssemblyPrototypeRecord gpuSceneAssemblyPrototype(
    GpuSceneAddressBlock addresses, GpuGeometryRecord geometry, uint index)
{
    GpuAssemblyPrototypeRecord* record = (GpuAssemblyPrototypeRecord*)(
        addresses.parts + geometry.parts.first + GPU_ASSEMBLY_HEADER_SIZE
        + uint64_t(index) * GPU_ASSEMBLY_PROTOTYPE_RECORD_SIZE);
    return *record;
}

// The use record at global index `useIndex` within `geometry`'s parts range. `prototypeCount` is
// the geometry's assembly prototype count (GpuGeometryRecord.reserved).
public GpuAssemblyUseRecord gpuSceneAssemblyUse(
    GpuSceneAddressBlock addresses,
    GpuGeometryRecord geometry,
    uint prototypeCount,
    uint useIndex)
{
    GpuAssemblyUseRecord* record = (GpuAssemblyUseRecord*)(
        addresses.parts + geometry.parts.first + GPU_ASSEMBLY_HEADER_SIZE
        + uint64_t(prototypeCount) * GPU_ASSEMBLY_PROTOTYPE_RECORD_SIZE
        + uint64_t(useIndex) * GPU_ASSEMBLY_USE_RECORD_SIZE);
    return *record;
}

// Whether `useIndex` draws for `combination` — bit `useIndex` of the combination's
// mask words, following the use table. A combination beyond the table clamps to the
// last authored entry.
public bool gpuSceneAssemblyUseActive(
    GpuSceneAddressBlock addresses,
    GpuGeometryRecord geometry,
    GpuAssemblyHeaderRecord header,
    uint combination,
    uint useIndex)
{
    if (header.combinationCount == 0u)
    {
        return true;
    }
    uint clamped = min(combination, header.combinationCount - 1u);
    uint64_t maskBase = addresses.parts + geometry.parts.first + GPU_ASSEMBLY_HEADER_SIZE
        + uint64_t(header.prototypeCount) * GPU_ASSEMBLY_PROTOTYPE_RECORD_SIZE
        + uint64_t(header.useCount) * GPU_ASSEMBLY_USE_RECORD_SIZE;
    uint word = *(uint*)(
        maskBase + uint64_t(clamped * header.maskWords + useIndex / 32u) * 4u);
    return (word & (1u << (useIndex % 32u))) != 0u;
}

public float3 gpuSceneAssemblyTransformPoint(GpuAssemblyUseRecord use, float3 local)
{
    float4 p = float4(local, 1.0);
    return float3(dot(use.row0, p), dot(use.row1, p), dot(use.row2, p));
}

// Rotation/scale only, so the caller normalizes; exact for the rigid/uniform transforms plant
// assemblies place.
public float3 gpuSceneAssemblyTransformVector(GpuAssemblyUseRecord use, float3 local)
{
    return float3(
        dot(use.row0.xyz, local), dot(use.row1.xyz, local), dot(use.row2.xyz, local));
}

// `proxyAlbedo` is the resolved base-color rgb packed 8:8:8 unorm (low to high) —
// the GI occluder's proxy color; high byte reserved. `occupancy` is the aggregate
// density the distance-field consumers march this matter with: 1 solid, fractional
// thin-sheet parity occupancy.
public struct GpuMaterialTableRecord
{
    public GpuHandle baseColorTexture;
    public GpuHandle normalTexture;
    public GpuHandle coverage;
    public uint parameterIndex;
    public uint materialClass;
    public uint shaderIndex;
    public uint flags;
    public uint proxyAlbedo;
    public float occupancy;
};

public float3 gpuMaterialProxyAlbedo(GpuMaterialTableRecord material)
{
    return float3(
        float(material.proxyAlbedo & 0xffu),
        float((material.proxyAlbedo >> 8u) & 0xffu),
        float((material.proxyAlbedo >> 16u) & 0xffu)) / 255.0;
}

// Scalar reserved words, not a `uint3` — see `GpuAssemblyUseRecord`; the block starts at offset
// 20, which is not 16-byte aligned.
public struct GpuTextureTableRecord
{
    public uint descriptorIndex;
    public uint width;
    public uint height;
    public uint mipCount;
    public uint flags;
    public uint reserved0;
    public uint reserved1;
    public uint reserved2;
};

public struct GpuCoverageRecord
{
    public GpuHandle texture;
    public float cutoff;
    public uint classification;
    public uint sourceKind;
    public uint ommPolicy;
    public uint2 hashSalt;
    public uint2 sourceExtent;
    public uint ommThresholds;
    public uint reserved;
};

public struct GpuSkeletonRecord
{
    public GpuArenaRange joints;
    public GpuArenaRange inverseBinds;
    public GpuArenaRange deformation;
    public uint flags;
    public uint reserved;
};

public struct GpuSkeletonJointRecord
{
    public uint parent;
    public uint flags;
};

public struct GpuInverseBindRecord
{
    public float4x4 matrix;
};

public struct GpuDeformationProviderRecord
{
    public uint providerMask;
    public uint firstParameter;
    public uint parameterCount;
    public uint flags;
};

public struct GpuPageRecord
{
    public GpuHandle parent;
    public GpuArenaRange dependencies;
    public uint64_t byteOffset;
    public uint byteLength;
    public uint residentGeneration;
    public uint flags;
    public uint reserved;
};

public struct GpuDrawRecord
{
    public GpuHandle geometry;
    public GpuHandle material;
    public GpuHandle instance;
    public GpuHandle deformation;
    public uint contentIndex;
    public uint part;
    public uint representation;
    public uint sourceGeneration;
    public uint psoBin;
    public uint transition;
    public uint clusterState;
    public uint reserved;
};

public static const uint GPU_SCENE_TRANSFORM_STATIC = 0u;
// The instance anchors a micro vegetation field: the visibility cull skips it, and only
// micro-blade records reference it.
public static const uint GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD = 1u;
// Instance-flag shift of the two-bit vegetation interaction policy.
public static const uint GPU_SCENE_INSTANCE_POLICY_SHIFT = 1u;
// The static payload carries explicit conservative bounds spheres (float4 words 4..6) the cull
// composes instead of the prototype sphere.
public static const uint GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS = 8u;
// The static payload carries a surface attachment identity (float4 words 6..7+).
public static const uint GPU_SCENE_INSTANCE_FLAG_ATTACHED = 16u;
// The wind deformation prepass writes this instance's sway record, every raster pass applies it,
// and the visibility cull adds its bounds slack.
public static const uint GPU_SCENE_INSTANCE_FLAG_WIND = 32u;
public static const uint GPU_SCENE_TRANSFORM_DYNAMIC = 1u;

public struct GpuScenePrototypeGpuRecord
{
    public GpuHandle geometry;
    public GpuArenaRange materialRange;
    public GpuHandle deformation;
    // Packed scene-SDF handles in the prototype-SDF arena, one per baked field of the prototype's
    // mesh (empty when it baked none). Many tight fields rather than one enclosing field, so each
    // culls independently.
    public GpuArenaRange sdfRange;
    public GpuHandle rootPage;
    public uint sourceGeneration;
    public uint flags;
    public float4 bounds;
    // The prototype's authored mechanical response in its cooked integer form: Q15.16
    // stiffness, drag, and flutter, then the [0,1] damping in the low half of the last
    // word and the bend limit in the high half. All zero outside a plant family.
    public uint4 mechanics;
};

// One plant family's authored wind and bend response, unpacked from the prototype record.
public struct GpuMechanicalResponse
{
    public float stiffness;
    public float drag;
    public float flutter;
    public float damping;
    public float bendLimit;
    // False outside a plant family, where the caller derives the response from height alone.
    public bool authored;
};

public GpuMechanicalResponse gpuSceneMechanicalResponse(GpuScenePrototypeGpuRecord prototype)
{
    GpuMechanicalResponse response;
    response.stiffness = float(int(prototype.mechanics.x)) / 65536.0;
    response.drag = float(int(prototype.mechanics.y)) / 65536.0;
    response.flutter = float(int(prototype.mechanics.z)) / 65536.0;
    response.damping = float(prototype.mechanics.w & 0xffffu) / 65535.0;
    response.bendLimit = float(prototype.mechanics.w >> 16u) / 65535.0;
    response.authored = (prototype.mechanics.x | prototype.mechanics.y | prototype.mechanics.z
        | prototype.mechanics.w) != 0u;
    return response;
}

public struct GpuSceneReferenceGpuRecord
{
    public GpuHandle target;
    public uint2 sourceRevision;
};

public struct GpuScenePageGpuRecord
{
    public GpuHandle table;
    public GpuHandle parent;
    public uint sourceGeneration;
    public uint flags;
};

public struct GpuSceneInstanceGpuRecord
{
    public GpuHandle prototype;
    public GpuHandle deformation;
    // Alignment padding keeping the transform payload 16-byte aligned.
    public uint2 reservedPad;
    public GpuArenaRange materialOverrides;
    public uint transformKind;
    public uint sourceGeneration;
    public uint flags;
    public uint reserved;
    public float4 transform[8];
};

// The world columns of one scene instance. A dynamic record stores explicit float columns
// (current at 0, previous at 4). A static record stores the exact compact placement — signed
// level-zero cell coordinates, cell-local position ticks, the quantized XYZW quaternion, and the
// Q15.16 per-axis scale — decoded here with no CPU float conversion in the path; a static point
// has no motion, so previous equals current.
public void gpuSceneInstanceColumns(
    GpuSceneInstanceGpuRecord instance,
    bool previous,
    out float4 c0,
    out float4 c1,
    out float4 c2,
    out float4 c3)
{
    if (instance.transformKind == GPU_SCENE_TRANSFORM_DYNAMIC)
    {
        uint base = previous ? 4u : 0u;
        c0 = instance.transform[base + 0u];
        c1 = instance.transform[base + 1u];
        c2 = instance.transform[base + 2u];
        c3 = instance.transform[base + 3u];
        return;
    }
    // The compact record's 16 words, reinterpreted from the column storage:
    // cell.x lo/hi, cell.y lo/hi | cell.z lo/hi, ticks.x, ticks.y | ticks.z,
    // orientation.xy, orientation.zw, scale.x | scale.y, scale.z, flags, reserved.
    uint4 w0 = asuint(instance.transform[0]);
    uint4 w1 = asuint(instance.transform[1]);
    uint4 w2 = asuint(instance.transform[2]);
    uint4 w3 = asuint(instance.transform[3]);
    float3 position = float3(
        float(int64_t((uint64_t(w0.y) << 32) | uint64_t(w0.x))) * 64.0
            + float(w1.z) * (1.0 / 4096.0),
        float(int64_t((uint64_t(w0.w) << 32) | uint64_t(w0.z))) * 64.0
            + float(w1.w) * (1.0 / 4096.0),
        float(int64_t((uint64_t(w1.y) << 32) | uint64_t(w1.x))) * 64.0
            + float(w2.x) * (1.0 / 4096.0));
    float4 q = float4(
        float(int(w2.y << 16u) >> 16) / 32767.0,
        float(int(w2.y) >> 16) / 32767.0,
        float(int(w2.z << 16u) >> 16) / 32767.0,
        float(int(w2.z) >> 16) / 32767.0);
    float lengthSq = dot(q, q);
    q = lengthSq > 1e-8 ? q * rsqrt(lengthSq) : float4(0.0, 0.0, 0.0, 1.0);
    float3 scale = float3(
        float(int(w2.w)) / 65536.0, float(int(w3.x)) / 65536.0, float(int(w3.y)) / 65536.0);
    float3 col0 = float3(
        1.0 - 2.0 * (q.y * q.y + q.z * q.z),
        2.0 * (q.x * q.y + q.z * q.w),
        2.0 * (q.x * q.z - q.y * q.w));
    float3 col1 = float3(
        2.0 * (q.x * q.y - q.z * q.w),
        1.0 - 2.0 * (q.x * q.x + q.z * q.z),
        2.0 * (q.y * q.z + q.x * q.w));
    float3 col2 = float3(
        2.0 * (q.x * q.z + q.y * q.w),
        2.0 * (q.y * q.z - q.x * q.w),
        1.0 - 2.0 * (q.x * q.x + q.y * q.y));
    c0 = float4(col0 * scale.x, 0.0);
    c1 = float4(col1 * scale.y, 0.0);
    c2 = float4(col2 * scale.z, 0.0);
    c3 = float4(position, 1.0);
}

public struct GpuSceneLightGpuRecord
{
    public float4 positionRange;
    public float4 colorIntensity;
    public float4 directionType;
    public float4 spotCos;
    public uint2 sourceRevision;
    public uint2 reserved;
};

public struct GpuSceneOverrideGpuRecord
{
    public uint slot;
    public uint reserved;
    public GpuHandle material;
};

public static const uint GPU_SCENE_PROTOTYPE_SLOT_STRIDE = 96u;
public static const uint GPU_SCENE_REFERENCE_SLOT_STRIDE = 32u;
public static const uint GPU_SCENE_PAGE_SLOT_STRIDE = 48u;
public static const uint GPU_SCENE_INSTANCE_SLOT_STRIDE = 192u;
public static const uint GPU_SCENE_LIGHT_SLOT_STRIDE = 96u;

public struct GpuSceneAddressBlock
{
    public uint64_t prototypes;
    public uint64_t geometries;
    public uint64_t materials;
    public uint64_t textures;
    public uint64_t coverage;
    public uint64_t skeletons;
    public uint64_t sdfs;
    public uint64_t pages;
    public uint64_t scenePrototypes;
    public uint64_t sceneMaterials;
    public uint64_t sceneDeformations;
    public uint64_t sceneSdfs;
    public uint64_t scenePages;
    public uint64_t instances;
    public uint64_t lights;
    public uint64_t overrides;
    public uint64_t prototypeMaterials;
    public uint64_t prototypeSdfs;
    public uint64_t materialParameters;
    public uint64_t vertices;
    public uint64_t indices;
    public uint64_t parts;
    public uint64_t submeshTable;
    public uint64_t pageBytes;
    public uint64_t pageRequests;
    public uint64_t deformedVertices;
    public uint64_t prevDeformedVertices;
    public uint64_t deformationProviders;
    public uint64_t deformationParameters;
    public uint64_t fields;
    public uint64_t candidates;
    public uint64_t windRecords;
    public uint64_t interactionField;
    public uint64_t quadCounters;
    public uint64_t displacedVertices;
    public uint64_t displacedPrevVertices;
    public uint64_t displacedIndices;
    public uint64_t displacedDraws;
    public uint64_t displacedRows;
    public uint64_t rayInstances;
    public uint instanceCapacity;
    public uint lightCapacity;
    public uint coverageTemporalPhase;
    public uint pageRequestCapacity;
    public uint pageRequestBudget;
    public uint pageRequestClasses;
    public uint displacedRowCount;
    public uint rayInstanceCount;
    public uint reserved0;
    public uint reserved1;
    public uint reserved2;
    public uint reserved3;
};

/// Counter word the geometry fragment shaders accumulate covered samples into.
public static const uint GPU_SCENE_COUNTER_COVERED_SAMPLES = 20u;

// The per-frame address block every übershader-family pass resolves persistent scene records
// through.
[[vk::binding(3, 2)]] public ConstantBuffer<GpuSceneAddressBlock> gpuSceneAddresses;

public GpuTableSlotHeader gpuSceneSlotHeader(uint64_t table, uint stride, uint slot)
{
    GpuTableSlotHeader* header = (GpuTableSlotHeader*)(table + uint64_t(slot) * stride);
    return *header;
}

public GpuSceneInstanceGpuRecord gpuSceneLoadInstance(GpuSceneAddressBlock addresses, uint slot)
{
    GpuSceneInstanceGpuRecord* record = (GpuSceneInstanceGpuRecord*)(
        addresses.instances + uint64_t(slot) * GPU_SCENE_INSTANCE_SLOT_STRIDE + 16u);
    return *record;
}

// One wind-deformed instance's prepass output: the full sway displacement at the instance's
// bounds top for the current and previous frame times, the reciprocal of the local bounds-top
// height (so a vertex derives its root-anchored weight without loading the prototype), and the
// world-space cull slack covering both sways.
public struct GpuWindInstanceRecord
{
    public float3 swayCurrent;
    public float heightScale;
    public float3 swayPrevious;
    public float boundsInflation;
    public float3 interactionCurrent;
    // 1.0 when the interaction cascade this instance reads changed since the previous frame: its
    // stored displacement jumped rather than moved, so reprojection would smear the jump and the
    // reactive-coverage pass marks the instance instead.
    public float interactionReset;
    public float3 interactionPrevious;
    public float reserved1;
    public float4 branchQuadrature;
    public float branchAmplitude;
    public float flutterAmplitude;
    public float2 reserved2;
};

public static const uint GPU_WIND_INSTANCE_RECORD_SIZE = 96u;

// Whether the instance in `slot` reads a discontinuous interaction displacement this
// frame — the prepass's verdict, read back by the reactive-coverage pass.
public bool gpuSceneWindInteractionReset(GpuSceneAddressBlock addresses, uint slot)
{
    if (addresses.windRecords == 0)
    {
        return false;
    }
    GpuWindInstanceRecord* record = (GpuWindInstanceRecord*)(
        addresses.windRecords + uint64_t(slot) * GPU_WIND_INSTANCE_RECORD_SIZE);
    return (*record).interactionReset != 0.0;
}

float3 gpuWindNormalizeOrZero(float3 value)
{
    float lengthSq = dot(value, value);
    return lengthSq > 1e-12 ? value * rsqrt(lengthSq) : float3(0.0, 0.0, 0.0);
}

// The wind deformation a raster vertex applies for its instance, from the prepass record alone
// (no field evaluation): the whole-plant sway tip-weighted, the interaction push root-leaning,
// and — for assembly uses with a moving structural semantic — the branch mode, a
// stored-quadrature oscillation about the use's pivot with a stable per-use phase. Leaf-family
// semantics add a double-frequency cross-wind flutter. Zero for instances outside the prepass.
public float3 gpuSceneWindDeform(
    GpuSceneAddressBlock addresses,
    GpuSceneInstanceGpuRecord instance,
    uint slot,
    float3 local,
    bool previous,
    bool assembly,
    GpuAssemblyUseRecord use,
    uint useIndex)
{
    if ((instance.flags & GPU_SCENE_INSTANCE_FLAG_WIND) == 0u || addresses.windRecords == 0)
    {
        return float3(0.0, 0.0, 0.0);
    }
    GpuWindInstanceRecord* recordPtr = (GpuWindInstanceRecord*)(
        addresses.windRecords + uint64_t(slot) * GPU_WIND_INSTANCE_RECORD_SIZE);
    GpuWindInstanceRecord record = *recordPtr;
    float weight = clamp(local.y * record.heightScale, 0.0, 1.0);
    float3 offset = (previous ? record.swayPrevious : record.swayCurrent) * (weight * weight)
        + (previous ? record.interactionPrevious : record.interactionCurrent) * weight;
    uint semantic = assembly ? use.reserved0 : 0u;
    bool moving = semantic >= 1u && semantic <= 6u && semantic != 2u;
    if (moving && record.branchAmplitude > 0.0)
    {
        float2 quadrature =
            previous ? record.branchQuadrature.zw : record.branchQuadrature.xy;
        float phase = float((useIndex * 0x9e3779b9u) >> 22u) * (6.2831853 / 1024.0);
        float oscillation = quadrature.x * cos(phase) + quadrature.y * sin(phase);
        float3 pivot = float3(use.row0.w, use.row1.w, use.row2.w);
        float lever = clamp(length(local - pivot), 0.0, 4.0);
        float3 along = gpuWindNormalizeOrZero(record.swayCurrent);
        float response = semantic >= 3u ? 1.0 : 0.6;
        offset += along * (oscillation * record.branchAmplitude * response * lever * 0.25);
        if (semantic >= 3u && record.flutterAmplitude > 0.0)
        {
            // Double the frequency through the quadrature identity and swing across the wind, so
            // foliage shimmers rather than pumps.
            float2 doubled = float2(
                2.0 * quadrature.x * quadrature.y,
                quadrature.y * quadrature.y - quadrature.x * quadrature.x);
            float flutter = doubled.x * cos(phase * 1.7) + doubled.y * sin(phase * 1.7);
            float3 across = float3(-along.z, 0.0, along.x);
            offset += across * (flutter * record.flutterAmplitude * weight);
        }
    }
    return offset;
}

public GpuScenePrototypeGpuRecord gpuSceneLoadPrototype(GpuSceneAddressBlock addresses, uint slot)
{
    GpuScenePrototypeGpuRecord* record = (GpuScenePrototypeGpuRecord*)(
        addresses.scenePrototypes + uint64_t(slot) * GPU_SCENE_PROTOTYPE_SLOT_STRIDE + 16u);
    return *record;
}

public GpuSceneReferenceGpuRecord gpuSceneLoadMaterialReference(
    GpuSceneAddressBlock addresses, uint slot)
{
    GpuSceneReferenceGpuRecord* record = (GpuSceneReferenceGpuRecord*)(
        addresses.sceneMaterials + uint64_t(slot) * GPU_SCENE_REFERENCE_SLOT_STRIDE + 16u);
    return *record;
}

public GpuSceneReferenceGpuRecord gpuSceneLoadSdfReference(
    GpuSceneAddressBlock addresses, uint slot)
{
    GpuSceneReferenceGpuRecord* record = (GpuSceneReferenceGpuRecord*)(
        addresses.sceneSdfs + uint64_t(slot) * GPU_SCENE_REFERENCE_SLOT_STRIDE + 16u);
    return *record;
}

public GpuSceneReferenceGpuRecord gpuSceneLoadDeformation(
    GpuSceneAddressBlock addresses, uint slot)
{
    GpuSceneReferenceGpuRecord* record = (GpuSceneReferenceGpuRecord*)(
        addresses.sceneDeformations + uint64_t(slot) * GPU_SCENE_REFERENCE_SLOT_STRIDE + 16u);
    return *record;
}

public GpuScenePageGpuRecord gpuSceneLoadPage(GpuSceneAddressBlock addresses, uint slot)
{
    GpuScenePageGpuRecord* record = (GpuScenePageGpuRecord*)(
        addresses.scenePages + uint64_t(slot) * GPU_SCENE_PAGE_SLOT_STRIDE + 16u);
    return *record;
}

public GpuSceneLightGpuRecord gpuSceneLoadLight(GpuSceneAddressBlock addresses, uint slot)
{
    GpuSceneLightGpuRecord* record = (GpuSceneLightGpuRecord*)(
        addresses.lights + uint64_t(slot) * GPU_SCENE_LIGHT_SLOT_STRIDE + 16u);
    return *record;
}

public GpuHandle gpuScenePrototypeSdfHandle(GpuSceneAddressBlock addresses, uint element)
{
    GpuHandle* handle = (GpuHandle*)(addresses.prototypeSdfs + uint64_t(element) * 8u);
    return *handle;
}

public GpuHandle gpuScenePrototypeMaterialHandle(GpuSceneAddressBlock addresses, uint element)
{
    GpuHandle* handle = (GpuHandle*)(addresses.prototypeMaterials + uint64_t(element) * 8u);
    return *handle;
}

public GpuSceneOverrideGpuRecord gpuSceneOverrideElement(
    GpuSceneAddressBlock addresses, uint element)
{
    GpuSceneOverrideGpuRecord* record = (GpuSceneOverrideGpuRecord*)(
        addresses.overrides + uint64_t(element) * 16u);
    return *record;
}

public GpuSubmeshRecord gpuSceneSubmeshRecord(GpuSceneAddressBlock addresses, uint element)
{
    GpuSubmeshRecord* record = (GpuSubmeshRecord*)(addresses.submeshTable + uint64_t(element) * 16u);
    return *record;
}

public static const uint GPU_DEFORMATION_PROVIDER_SKINNING = 1u;

public GpuDeformationProviderRecord gpuSceneDeformationProvider(
    GpuSceneAddressBlock addresses, uint element)
{
    GpuDeformationProviderRecord* record = (GpuDeformationProviderRecord*)(
        addresses.deformationProviders + uint64_t(element) * 16u);
    return *record;
}

public uint gpuSceneDeformationParameter(GpuSceneAddressBlock addresses, uint element)
{
    uint* word = (uint*)(addresses.deformationParameters + uint64_t(element) * 4u);
    return *word;
}

// Bytes of one amplified micro-vertex in the displacement arena: position, normal, uv, tangent.
public static const uint GPU_DISPLACED_VERTEX_SIZE = 48u;

// The row a displaced instance owns in the frame's amplification arena, paired with the
// instance slot the traversal walks. Sorted by slot so the lookup is a binary search.
public struct GpuDisplacedRow
{
    public uint slot;
    public uint row;
};

// The amplification row for `slot`, or false when the instance is not displaced this frame.
// The rows are sorted by slot, which is what makes one comparison per step sound.
public bool gpuSceneDisplacedRow(GpuSceneAddressBlock addresses, uint slot, out uint row)
{
    row = 0u;
    if (addresses.displacedRows == 0)
    {
        return false;
    }
    GpuDisplacedRow* rows = (GpuDisplacedRow*)(addresses.displacedRows);
    uint low = 0u;
    uint high = addresses.displacedRowCount;
    while (low < high)
    {
        uint mid = (low + high) / 2u;
        let entry = rows[mid];
        if (entry.slot == slot)
        {
            row = entry.row;
            return true;
        }
        if (entry.slot < slot)
        {
            low = mid + 1u;
        }
        else
        {
            high = mid;
        }
    }
    return false;
}

// One displaced row's amplified draw: the exact index count the emit kernel packed plus its
// slice bases (five words per row, the `VkDrawIndexedIndirectCommand` layout the finalize
// kernel writes). `firstInstance` is the binner's to fill, so it is not read here.
public void gpuSceneDisplacedDraw(
    GpuSceneAddressBlock addresses,
    uint row,
    out uint indexCount,
    out uint firstIndex,
    out uint vertexBase)
{
    uint* seed = (uint*)(addresses.displacedDraws + uint64_t(row) * 20u);
    indexCount = seed[0];
    firstIndex = seed[2];
    vertexBase = seed[3];
}

// One amplified micro-vertex, current frame or the previous-frame geomorph slide the emit kernel
// wrote into the parallel stream.
//
// The 48-byte micro-vertex is packed (position 0, normal 12, uv 24, tangent 32), so the fields load
// through scalar pointers: a struct read would be a Block in PhysicalStorageBuffer whose second
// `float3` sits at offset 12, and std430 requires a three-component vector on a 16-byte boundary.
public void gpuSceneDisplacedVertex(
    GpuSceneAddressBlock addresses,
    uint vertex,
    bool previous,
    out float3 position,
    out float3 normal,
    out float2 uv)
{
    uint64_t base = (previous ? addresses.displacedPrevVertices : addresses.displacedVertices)
        + uint64_t(vertex) * GPU_DISPLACED_VERTEX_SIZE;
    position = *(float3*)(base);
    normal = *(float3*)(base + 12u);
    uv = *(float2*)(base + 24u);
}

public static const uint GPU_PAGE_TABLE_STRIDE = 64u;

public GpuPageRecord gpuSceneResidentPage(GpuSceneAddressBlock addresses, GpuHandle handle)
{
    GpuPageRecord* record = (GpuPageRecord*)(
        addresses.pages + uint64_t(handle.index) * GPU_PAGE_TABLE_STRIDE + 16u);
    return *record;
}

public static const uint GPU_PAGE_NODE_RECORD_SIZE = 96u;
public static const uint GPU_PAGE_CLUSTER_RECORD_SIZE = 48u;
public static const uint GPU_PAGE_VOXEL_VERTEX_SIZE = 16u;
public static const uint GPU_PAGE_PAYLOAD_FLAG_GUARANTEED_ROOT = 1u;

public struct GpuPageNodeRecord
{
    public uint representation;
    public uint clusterCount;
    public uint childCount;
    public uint flags;
    public float3 boundsMin;
    public uint appearanceTotal;
    public float3 boundsMax;
    public uint transitionTotal;
    public float3 deformedMin;
    public uint vertexCount;
    public float3 deformedMax;
    public uint indexCount;
    // The single source prototype of every drawable in the subtree, or
    // GPU_PAGE_NODE_NO_PROTOTYPE when it spans prototypes (the family root). The
    // traversal forks assembly uses on it.
    public uint prototype;
    public uint reserved0;
    public uint reserved1;
    public uint reserved2;
};

// GpuPageNodeRecord.prototype: the subtree spans prototypes (no assembly fork).
public static const uint GPU_PAGE_NODE_NO_PROTOTYPE = 0xFFFFFFFFu;

public struct GpuPageClusterRecord
{
    public uint firstIndex;
    public uint indexCount;
    public uint materialSlot;
    public uint prototype;
    public float3 boundsMin;
    public uint appearanceTotal;
    public float3 boundsMax;
    public uint cone;
};

public struct GpuPageVoxelVertex
{
    public float3 position;
    public uint normalOct;
};

// Payload readers over a resident page's byte span in the global page arena. The child handle
// table follows the node header; cluster records start at the next 16-byte boundary; a voxel
// page's vertex block and the u32 index blob follow the cluster records.
public GpuPageNodeRecord gpuScenePageNode(GpuSceneAddressBlock addresses, GpuPageRecord page)
{
    GpuPageNodeRecord* record = (GpuPageNodeRecord*)(addresses.pageBytes + page.byteOffset);
    return *record;
}

public GpuHandle gpuScenePageChild(GpuSceneAddressBlock addresses, GpuPageRecord page, uint child)
{
    GpuHandle* handle = (GpuHandle*)(
        addresses.pageBytes + page.byteOffset + GPU_PAGE_NODE_RECORD_SIZE + child * 8u);
    return *handle;
}

public uint64_t gpuScenePagePayloadBase(GpuPageRecord page, GpuPageNodeRecord node)
{
    uint offset = GPU_PAGE_NODE_RECORD_SIZE + node.childCount * 8u;
    return page.byteOffset + ((offset + 15u) & ~15u);
}

public GpuPageClusterRecord gpuScenePageCluster(
    GpuSceneAddressBlock addresses, GpuPageRecord page, GpuPageNodeRecord node, uint cluster)
{
    GpuPageClusterRecord* record = (GpuPageClusterRecord*)(
        addresses.pageBytes + gpuScenePagePayloadBase(page, node)
        + cluster * GPU_PAGE_CLUSTER_RECORD_SIZE);
    return *record;
}

public GpuPageVoxelVertex gpuScenePageVoxelVertex(
    GpuSceneAddressBlock addresses, GpuPageRecord page, GpuPageNodeRecord node, uint vertex)
{
    GpuPageVoxelVertex* record = (GpuPageVoxelVertex*)(
        addresses.pageBytes + gpuScenePagePayloadBase(page, node)
        + node.clusterCount * GPU_PAGE_CLUSTER_RECORD_SIZE
        + vertex * GPU_PAGE_VOXEL_VERTEX_SIZE);
    return *record;
}

// Appends one missing-page request — a resident page-table slot — to this frame's request buffer.
//
// The buffer is partitioned by view class: a count word per class, then one region per class, so a
// class's drops depend on its own volume rather than on atomic order against the other views.
//
// The region base uses the ALLOCATED capacity while the bound test uses the budget, which a caller
// may set lower. Addressing off the budget would move every region the moment it changed and the
// CPU drain would read garbage. A full region drops the request, and the count word keeps counting
// so the drain can price what was lost.
public void gpuSceneRequestPage(GpuSceneAddressBlock addresses, uint pageSlot, uint viewClass)
{
    uint region = min(viewClass, addresses.pageRequestClasses - 1u);
    Atomic<uint>* count = (Atomic<uint>*)(addresses.pageRequests + uint64_t(region) * 4u);
    uint index = count[0].add(1u);
    if (index < addresses.pageRequestBudget)
    {
        uint64_t base = addresses.pageRequests
            + uint64_t(addresses.pageRequestClasses) * 4u
            + uint64_t(region) * uint64_t(addresses.pageRequestCapacity) * 4u;
        uint* entry = (uint*)(base + uint64_t(index) * 4u);
        *entry = pageSlot;
    }
}

public static const uint GPU_GEOMETRY_TABLE_STRIDE = 80u;
public static const uint GPU_MATERIAL_TABLE_STRIDE = 64u;
public static const uint GPU_TEXTURE_TABLE_STRIDE = 48u;
public static const uint GPU_COVERAGE_TABLE_STRIDE = 64u;
public static const uint GPU_MATERIAL_PARAMS_STRIDE = 256u;

public GpuGeometryRecord gpuSceneResidentGeometry(GpuSceneAddressBlock addresses, GpuHandle handle)
{
    GpuGeometryRecord* record = (GpuGeometryRecord*)(
        addresses.geometries + uint64_t(handle.index) * GPU_GEOMETRY_TABLE_STRIDE + 16u);
    return *record;
}

public GpuMaterialTableRecord gpuSceneResidentMaterial(
    GpuSceneAddressBlock addresses, GpuHandle handle)
{
    GpuMaterialTableRecord* record = (GpuMaterialTableRecord*)(
        addresses.materials + uint64_t(handle.index) * GPU_MATERIAL_TABLE_STRIDE + 16u);
    return *record;
}

public GpuTextureTableRecord gpuSceneResidentTexture(
    GpuSceneAddressBlock addresses, GpuHandle handle)
{
    GpuTextureTableRecord* record = (GpuTextureTableRecord*)(
        addresses.textures + uint64_t(handle.index) * GPU_TEXTURE_TABLE_STRIDE + 16u);
    return *record;
}

public GpuCoverageRecord gpuSceneResidentCoverage(GpuSceneAddressBlock addresses, GpuHandle handle)
{
    GpuCoverageRecord* record = (GpuCoverageRecord*)(
        addresses.coverage + uint64_t(handle.index) * GPU_COVERAGE_TABLE_STRIDE + 16u);
    return *record;
}

// One baked signed distance field: the grid placement and atlas identity the occluder scatter
// composes an SdfInstance from. `localMin.w` is the R16_SNORM distance normalization clamp in
// local units, `voxelDims.w` the shared bindless SDF slot, `indirectionDims.w` the prefiltered
// atlas mip count; the corners are the padded grid bounds in local (rest) space.
public struct GpuSdfTableRecord
{
    public float4 localMin;
    public float4 localMax;
    public uint4 voxelDims;
    public uint4 indirectionDims;
    public uint4 atlasBricks;
};

public static const uint GPU_SDF_TABLE_STRIDE = 96u;

public GpuSdfTableRecord gpuSceneResidentSdf(GpuSceneAddressBlock addresses, GpuHandle handle)
{
    GpuSdfTableRecord* record = (GpuSdfTableRecord*)(
        addresses.sdfs + uint64_t(handle.index) * GPU_SDF_TABLE_STRIDE + 16u);
    return *record;
}

// One ray candidate resolved against the scene tables: the interpolated coverage inputs plus the
// records the sampler needs. `valid` is false when the instance slot is unoccupied or carries no
// coverage record, and the candidate then commits as covered.
public struct GpuSceneCandidateSurface
{
    public bool valid;
    public float2 uv;
    public float3 anchor;
    public GpuCoverageRecord coverage;
    public bool canonicalProbability;
    public float baseColorAlpha;
};

// One TLAS instance's identity, indexed by the `instanceCustomIndex` the packer wrote. A traced
// candidate carries a bottom-level structure and a geometry index, neither of which names a scene
// record on its own; this table is what turns the pair into one.
//
// `firstSubmesh` is the geometry's submesh element for geometry index 0. A plain mesh builds one
// geometry per submesh from element zero; an assembly prototype builds one per submesh of ITS span,
// so the span's start is what rebases the candidate's geometry index onto the family's table.
public struct GpuRayInstanceRecord
{
    public uint instanceSlot;
    public uint firstSubmesh;
    public uint reserved0;
    public uint reserved1;
};

public static const uint GPU_RAY_INSTANCE_RECORD_SIZE = 16u;

// The instance slot of a ray instance with no mirrored identity: its candidates commit without
// record resolution.
public static const uint GPU_RAY_INSTANCE_UNMIRRORED = 0xFFFFFFFFu;

public GpuRayInstanceRecord gpuSceneRayInstance(GpuSceneAddressBlock addresses, uint index)
{
    GpuRayInstanceRecord record;
    record.instanceSlot = GPU_RAY_INSTANCE_UNMIRRORED;
    record.firstSubmesh = 0u;
    record.reserved0 = 0u;
    record.reserved1 = 0u;
    if (addresses.rayInstances == 0 || index >= addresses.rayInstanceCount)
    {
        return record;
    }
    GpuRayInstanceRecord* stored = (GpuRayInstanceRecord*)(
        addresses.rayInstances + uint64_t(index) * GPU_RAY_INSTANCE_RECORD_SIZE);
    return *stored;
}

public GpuSceneCandidateSurface gpuSceneResolveCandidate(
    GpuSceneAddressBlock addresses,
    uint instanceSlot,
    uint firstSubmesh,
    uint geometryIndex,
    uint primitive,
    float2 barycentrics)
{
    GpuSceneCandidateSurface surface;
    surface.valid = false;
    surface.uv = float2(0.0);
    surface.anchor = float3(0.0);
    surface.canonicalProbability = false;
    surface.baseColorAlpha = 1.0;

    if (instanceSlot >= addresses.instanceCapacity)
    {
        return surface;
    }
    let header = gpuSceneSlotHeader(addresses.instances, GPU_SCENE_INSTANCE_SLOT_STRIDE, instanceSlot);
    if (header.occupied == 0u)
    {
        return surface;
    }
    let instance = gpuSceneLoadInstance(addresses, instanceSlot);
    let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);
    let geometry = gpuSceneResidentGeometry(addresses, prototype.geometry);

    // The candidate's geometry names its submesh exactly: the build lays one geometry per submesh,
    // in submesh order, so `firstSubmesh + geometryIndex` is the record and the primitive is
    // relative to that submesh's own index slice. Searching the table for a containing range
    // instead would resolve the wrong submesh for every geometry after the first, since a
    // per-geometry primitive index is not a position in the shared stream.
    uint submeshElement = firstSubmesh + geometryIndex;
    if (submeshElement >= geometry.submeshes.count)
    {
        return surface;
    }
    let submesh = gpuSceneSubmeshRecord(addresses, geometry.submeshes.first + submeshElement);
    if (primitive * 3u + 2u >= submesh.indexCount)
    {
        return surface;
    }
    uint firstIndex = submesh.firstIndex + primitive * 3u;
    if (firstIndex + 2u >= geometry.indices.count / 4u)
    {
        return surface;
    }
    uint64_t indexBase = addresses.indices + geometry.indices.first;
    uint* indexData = (uint*)indexBase;
    uint i0 = indexData[firstIndex];
    uint i1 = indexData[firstIndex + 1u];
    uint i2 = indexData[firstIndex + 2u];
    uint materialSlot = submesh.materialSlot;

    // Object-space position + UV interpolated exactly as the raster path's inputs.
    uint64_t vertexBase = addresses.vertices + geometry.vertices.first;
    float3 p0 = *(float3*)(vertexBase + uint64_t(i0) * geometry.vertexStride);
    float3 p1 = *(float3*)(vertexBase + uint64_t(i1) * geometry.vertexStride);
    float3 p2 = *(float3*)(vertexBase + uint64_t(i2) * geometry.vertexStride);
    float2 t0 = *(float2*)(vertexBase + uint64_t(i0) * geometry.vertexStride + 24u);
    float2 t1 = *(float2*)(vertexBase + uint64_t(i1) * geometry.vertexStride + 24u);
    float2 t2 = *(float2*)(vertexBase + uint64_t(i2) * geometry.vertexStride + 24u);
    float w0 = 1.0 - barycentrics.x - barycentrics.y;
    surface.anchor = p0 * w0 + p1 * barycentrics.x + p2 * barycentrics.y;
    surface.uv = t0 * w0 + t1 * barycentrics.x + t2 * barycentrics.y;

    // Material: the sparse instance override for this slot, else the prototype default.
    GpuHandle sceneMaterial = gpuScenePrototypeMaterialHandle(
        addresses, prototype.materialRange.first + min(materialSlot, prototype.materialRange.count - 1u));
    for (uint element = 0u; element < instance.materialOverrides.count; ++element)
    {
        let override = gpuSceneOverrideElement(addresses, instance.materialOverrides.first + element);
        if (override.slot == materialSlot)
        {
            sceneMaterial = override.material;
            break;
        }
    }
    let materialReference = gpuSceneLoadMaterialReference(addresses, sceneMaterial.index);
    let material = gpuSceneResidentMaterial(addresses, materialReference.target);
    if (material.coverage.index == 0xFFFFFFFFu)
    {
        return surface;
    }
    surface.coverage = gpuSceneResidentCoverage(addresses, material.coverage);
    surface.canonicalProbability =
        ((material.materialClass >> GPU_MATERIAL_SURFACE_MODEL_SHIFT) & 0x1u) != 0u;
    float* baseColorAlpha = (float*)(
        addresses.materialParameters
        + uint64_t(material.parameterIndex) * GPU_MATERIAL_PARAMS_STRIDE + 12u);
    surface.baseColorAlpha = *baseColorAlpha;
    surface.valid = true;
    return surface;
}

// Confirms one non-opaque ray candidate through the canonical coverage classifier: the ray
// instance names the scene record and submesh span, the candidate's UV and object-space anchor come
// from the scene tables, its coverage source is sampled at explicit LOD from the caller's bindless
// albedo array, and the shared classifier decides. An unmirrored instance or a surface without a
// coverage record counts as covered.
public bool gpuSceneRayCandidateCovered(
    GpuSceneAddressBlock addresses,
    Sampler2D albedo[1024],
    uint rayInstance,
    uint geometryIndex,
    uint primitive,
    float2 barycentrics)
{
    let identity = gpuSceneRayInstance(addresses, rayInstance);
    let surface = gpuSceneResolveCandidate(
        addresses,
        identity.instanceSlot,
        identity.firstSubmesh,
        geometryIndex,
        primitive,
        barycentrics);
    if (!surface.valid)
    {
        return true;
    }
    let texture = gpuSceneResidentTexture(addresses, surface.coverage.texture);
    float sampled = albedo[NonUniformResourceIndex(texture.descriptorIndex)]
        .SampleLevel(surface.uv, 0.0)
        .a;
    CoverageSample coverage = classifyCanonicalCoverage(
        sampled,
        surface.uv,
        surface.anchor,
        surface.coverage.sourceKind,
        surface.coverage.classification,
        surface.baseColorAlpha,
        surface.coverage.sourceExtent,
        surface.coverage.hashSalt,
        addresses.coverageTemporalPhase,
        surface.coverage.cutoff,
        0.0,
        surface.canonicalProbability,
        false);
    return coverage.covered;
}

/// Counts one covered sample, when something is measuring.
///
/// A helper invocation's atomics are discarded by the spec, so this counts only lanes that really
/// covered a sample, while the `FRAGMENT_SHADER_INVOCATIONS` pipeline statistic counts every lane
/// including helpers. Their ratio is quad utilization — the fraction of each shaded 2x2 quad that
/// was not wasted, which no pipeline statistic reports on its own and which sliver-shaped foliage
/// destroys. A zero address means nothing is measuring.
public void gpuSceneCountCoveredSample(GpuSceneAddressBlock addresses)
{
    if (addresses.quadCounters == 0)
    {
        return;
    }
    uint* counters = (uint*)addresses.quadCounters;
    uint previous;
    InterlockedAdd(counters[GPU_SCENE_COUNTER_COVERED_SAMPLES], 1u, previous);
}
