module mdf_brick;

// The per-mesh Mesh-Distance-Field (MDF) brick sample, factored into one place so every
// consumer reads the sparse SDST v3 bricks through a single implementation: the lighting
// übershader's cone trace (`sdf.slang::sdfSample`) and the Global-SDF composite
// (`gdf_composite.slang`) both call `sampleMdfBrick`. The bindless brick bindings (set 0)
// travel with the linked function, so every pipeline that links this module provides the
// bindless set.

// Bindless: the per-mesh signed-distance-field bricks (SDST v3), three parallel arrays at the
// same slot (set 0). Binding 1 is the brick *atlas* — an `R16_SNORM` Texture3D of only the
// occupied 8³ bricks, carrying a 3-mip prefiltered chain, sampled with a linear clamp-to-edge
// sampler (LINEAR mipmap mode so a fractional `SampleLevel` LOD blends across the mip pair).
// Binding 2 is the brick *indirection* volume — an `R32_UINT` Texture3D, one texel per brick
// mapping to an atlas brick index or EMPTY, read by integer `Load` (no sampler). Binding 3 is
// the coarse *coverage* volume — an `R16_SNORM` Texture3D, one texel per brick holding the
// conservative distance to the nearest surface in that block (the empty-space oracle),
// linearly sampled. Indexed by an SdfInstance's bindless slot (params.x). Size matches
// MAX_BINDLESS_SDF (descriptors.rs); partially bound + update-after-bind.
[[vk::binding(1, 0)]] Sampler3D sdfTextures[256];
[[vk::binding(2, 0)]] Texture3D<uint> sdfIndirection[256];
[[vk::binding(3, 0)]] Sampler3D sdfCoverage[256];

// Voxels per brick axis and the unique-voxel stride (the 8th voxel is the shared border).
static const uint kBrickSize = 8u;
static const float kBrickUseful = 7.0;
static const uint kEmptyBrick = 0xFFFFFFFFu;

// Set 1, binding 8 (in `sdf.slang`): the per-mesh SDF-occluder instance. Each entry maps a
// world sample into a baked field's local grid (worldToLocal), bounds it by a world AABB (cull)
// + a local AABB (the grid mapping), and names the bindless SDF slot + the v3 brick locators
// (fine dims, indirection dims, atlas tiling, encode clamp, scale). Defined here (with the
// brick sample) so every consumer shares the one struct definition.
public struct SdfInstance
{
    public float4x4 worldToLocal;  // inverse model: world sample -> local grid space
    public float4 worldMin;        // xyz = world AABB min (per-sample cull); w reserved
    public float4 worldMax;        // xyz = world AABB max; w reserved
    public float4 localMin;        // xyz = grid lower corner, local space; w reserved
    public float4 localMax;        // xyz = grid upper corner, local space; w reserved
    public float4 params;          // x = bindless SDF slot, y = max-encode dist (local), z = world scale, w = atlas mip count
    public uint4 voxelDims;        // xyz = fine voxel dims; w reserved
    public uint4 indirDims;        // xyz = brick indirection dims; w reserved
    public uint4 atlasBricks;      // xyz = atlas bricks per axis; w reserved
};

// The result of one field probe along a cone: the closest occluder distance (WORLD units) and
// an openness factor for the coverage-driven early-out.
public struct SdfSample
{
    public float distance;   // distance to the nearest occluder (world); 1e30 when the sample is outside the field
    public float openness;   // [0,1] coverage ahead (1 = block fully open); 0 near a surface
};

// Samples the brick atlas at one integer mip `level`. A brick holds 8 texels at mip 0 (7 useful
// + 1 shared border) and halves per coarser mip (8 -> 4 -> 2). The coarse mips are baked
// **corner-aligned** (see `Sdf::atlas_image_data_mip`): a coarse brick samples its `[0, 7]` fine
// span at evenly spaced points, so `inBrick` maps to brick-local texel `inBrick * (brickTexels -
// 1) / 7` — texel 0 and texel `brickTexels - 1` land exactly on the brick's two shared border
// voxels. Because the bake fills those border texels from the identical global window in both
// neighbours, the reconstruction is seam-free across brick boundaries at *every* mip.
float sampleBrickLevel(uint slot, uint3 bc, uint3 ab, float3 inBrick, int level)
{
    float brickTexels = float(kBrickSize) / exp2(float(level));  // 8, 4, 2 texels per brick axis
    float maxTexel = brickTexels - 1.0;
    float3 localTexel = clamp(inBrick * (maxTexel / kBrickUseful), float3(0.0), float3(maxTexel));
    float3 atlasDims = float3(ab) * brickTexels;
    float3 coord = float3(bc) * brickTexels + localTexel;
    float3 uvw = (coord + 0.5) / atlasDims;
    return sdfTextures[NonUniformResourceIndex(slot)].SampleLevel(uvw, float(level)).r;
}

// Probes one SDF occluder at a world sample given the cone-footprint radius (`footprint`, world
// units) at that point. The instance whose world AABB contains the sample is mapped into its
// local fine-voxel grid and the owning 8³ brick resolved through the indirection volume — one
// integer `Load`, the gate that decides the rest of the work:
//
//   - **Empty brick** (open-space common case): one coverage tap gives the block's conservative
//     open distance + openness. No atlas tap.
//   - **Occupied brick**: a surface is near, so it is not open (openness 0) and the brick atlas
//     is sampled with a cone-footprint mip level — `lod = log2(footprint / voxelSize)` — so a
//     widening cone reads a coarser, pre-low-passed mip (the anti-alias fix).
//
// The stored R16_SNORM is denormalized by the max-encode distance (local, params.y) then scaled
// to world units (params.z). Returns `distance = 1e30` (no contribution) when the sample falls
// outside this instance's padded grid bounds, so a caller can `min()` over many instances.
public SdfSample sampleMdfBrick(SdfInstance inst, float3 wp, float footprint)
{
    SdfSample s;
    s.distance = 1e30;
    s.openness = 1.0;
    // World-AABB cull: skip an instance the sample is not inside (the field is only valid within
    // its padded grid bounds; outside it, the box test rejects it).
    if (any(wp < inst.worldMin.xyz) || any(wp > inst.worldMax.xyz)) { return s; }
    // World sample -> local grid -> continuous fine-voxel coordinate (voxel-centered).
    float3 local = mul(inst.worldToLocal, float4(wp, 1.0)).xyz;
    float3 dims = float3(inst.voxelDims.xyz);
    float3 cell = (inst.localMax.xyz - inst.localMin.xyz) / max(dims, 1.0);
    float3 g = (local - inst.localMin.xyz) / cell - 0.5;
    float3 gMax = dims - 1.0;
    if (any(g < -0.5) || any(g > gMax + 0.5)) { return s; }
    g = clamp(g, float3(0.0), gMax);

    uint slot = uint(inst.params.x);
    float maxDist = inst.params.y;
    float worldScale = inst.params.z;
    uint mipCount = max(uint(inst.params.w), 1u);

    int3 indirDims = int3(inst.indirDims.xyz);
    int3 brick = clamp(int3(floor(g / kBrickUseful)), int3(0), indirDims - 1);
    float3 inBrick = g - float3(brick) * kBrickUseful; // [0, 7] within the brick

    uint atlasSlot = sdfIndirection[NonUniformResourceIndex(slot)].Load(int4(brick, 0)).x;
    float dLocal;
    if (atlasSlot == kEmptyBrick)
    {
        // Empty block: one coverage tap. Continuous brick coordinate g/7 maps the fine-voxel
        // position onto the coverage grid (coverage_dims == indirection_dims). The coverage
        // distance is the conservative (overshoot-safe) leap and the openness for the early-out.
        float3 covUVW = (g / kBrickUseful) / float3(inst.indirDims.xyz);
        float covVal = sdfCoverage[NonUniformResourceIndex(slot)].SampleLevel(covUVW, 0.0).r;
        s.openness = covVal;
        dLocal = covVal * maxDist;
    }
    else
    {
        // Occupied block: a surface is near, so it is not open (block the cone early-out) and
        // the precise atlas distance is required.
        s.openness = 0.0;
        uint3 ab = inst.atlasBricks.xyz;
        uint3 bc = uint3(atlasSlot % ab.x, (atlasSlot / ab.x) % ab.y, atlasSlot / (ab.x * ab.y));
        float voxelWorld = max(max(cell.x, cell.y), cell.z) * worldScale;
        float lodf = clamp(log2(footprint / max(voxelWorld, 1e-6)), 0.0, float(mipCount) - 1.0);
        int loLevel = int(floor(lodf));
        float frac = lodf - float(loLevel);
        float dNorm = sampleBrickLevel(slot, bc, ab, inBrick, loLevel);
        if (frac > 1e-3)
        {
            // Fractional LOD: blend the next coarser mip (quadrilinear). Skipped when the
            // footprint sits at or below a voxel (lod clamps to 0, frac 0), the near-field case.
            int hiLevel = min(loLevel + 1, int(mipCount) - 1);
            float dHi = sampleBrickLevel(slot, bc, ab, inBrick, hiLevel);
            dNorm = lerp(dNorm, dHi, frac);
        }
        dLocal = dNorm * maxDist;
    }
    s.distance = dLocal * worldScale;
    return s;
}
