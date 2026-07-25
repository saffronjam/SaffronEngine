// Global-SDF composite: for each dirty voxel of one cascade, samples every culled MDF brick
// covering that voxel and keeps the minimum signed distance, writing the toroidally-addressed
// `R16_SNORM` cascade volume. Empty voxels (no brick covers them) store the conservative open
// distance (+max-encode, never zero) so the sphere-march leaps across empty space safely. The
// dispatch covers only the dirty slab(s) the recenter scrolled in (global voxel coords; the write
// wraps modulo the resolution), so a moving camera pays a slab, not a full rebuild.

import mdf_brick;

// Set 1, binding 0: the per-mesh SDF instances. Binding 1: the cull list (per-cascade counters +
// index segments). Binding 2: the `kGdfCascades` cascade volumes (toroidal `R16_SNORM` storage),
// written here. Binding 3: the lite per-cell albedo cache (toroidal `rgba16f` storage, aligned to
// the finest cascade), written only for cascade 0 — a flat per-cell base color the DDGI trace
// reads as the hit radiance's albedo. This is the documented fidelity cap: no normal, no
// view-dependent shading, no emissive, no multi-material resolution within a cell — explicitly
// NOT a Surface Cache.
[[vk::binding(0, 1)]] StructuredBuffer<SdfInstance> instances;
[[vk::binding(1, 1)]] StructuredBuffer<uint> cullList;
[[vk::image_format("r16_snorm")]]
[[vk::binding(2, 1)]] RWTexture3D<float> cascades[3];
[[vk::image_format("rgba16f")]]
[[vk::binding(3, 1)]] RWTexture3D<float4> albedoCache;
// Binding 4: the per-cascade porous-occupancy volumes (toroidal `r8` density). Porous matter
// (instance occupancy < 1) never hardens the distance field; it splats density here instead,
// and the consumers march through it accumulating extinction.
[[vk::image_format("r8")]]
[[vk::binding(4, 1)]] RWTexture3D<float> occupancyCascades[3];

// Header words before the per-cascade index segments (matches `gdf_cull.slang` / `global_sdf.rs`).
static const uint kCullHeader = 4u;

struct Push
{
    int4 globalBase;    // xyz = region min in global voxel coords, w = cascade index
    uint4 size;         // xyz = region dims (voxels), w = max culled per cascade
    float4 cascadeInfo; // x = voxel size (world), y = max-encode dist (world), z = resolution, w reserved
};
[[vk::push_constant]] Push pc;

// Euclidean modulo for the toroidal wrap (HLSL `%` is truncated; negative global cells must wrap
// to a positive physical texel).
int wrapMod(int v, int n)
{
    int r = v % n;
    return (r < 0) ? r + n : r;
}

[shader("compute")]
[numthreads(4, 4, 4)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    if (any(tid >= pc.size.xyz)) { return; }

    uint c = uint(pc.globalBase.w);
    int res = int(pc.cascadeInfo.z);
    float voxel = pc.cascadeInfo.x;
    float maxEncode = pc.cascadeInfo.y;
    uint maxCulled = pc.size.w;

    int3 global = pc.globalBase.xyz + int3(tid);
    // The voxel's world-space center (global cell index, independent of the toroidal storage).
    float3 worldCenter = (float3(global) + 0.5) * voxel;

    // Composite at the cascade's voxel scale so the brick mip-select band-limits the MDF into the
    // GDF voxel (alias-free downsample).
    float footprint = voxel;
    uint count = min(cullList[c], maxCulled);
    float best = 1e30;
    float3 bestAlbedo = float3(0.0);
    float porous = 0.0;
    float3 porousAlbedo = float3(0.0);
    for (uint k = 0; k < count; k = k + 1)
    {
        uint idx = cullList[kCullHeader + c * maxCulled + k];
        SdfInstance inst = instances[idx];
        SdfSample smp = sampleMdfBrick(inst, worldCenter, footprint);
        float instanceOccupancy = inst.localMax.w;
        if (instanceOccupancy < 1.0)
        {
            // Porous matter: density where the brick surface passes through the voxel,
            // never a hard distance. The densest overlapping instance wins.
            if (smp.distance < voxel * 0.5 && instanceOccupancy > porous)
            {
                porous = instanceOccupancy;
                porousAlbedo = float3(inst.worldMin.w, inst.worldMax.w, inst.localMin.w);
            }
            continue;
        }
        if (smp.distance < best)
        {
            best = smp.distance;
            // The instance's base color rides the reserved `.w` of its world/local AABB corners
            // (render_scene.rs packs it there; the brick sample reads only `.xyz`).
            bestAlbedo = float3(inst.worldMin.w, inst.worldMax.w, inst.localMin.w);
        }
    }

    // Empty voxel -> conservative open distance (+max-encode = +1 normalized), never zero, so the
    // march never stalls. Occupied -> the min signed distance, normalized + clamped to the encode.
    float stored = (best >= 1e29) ? 1.0 : clamp(best / maxEncode, -1.0, 1.0);

    int3 phys = int3(wrapMod(global.x, res), wrapMod(global.y, res), wrapMod(global.z, res));
    cascades[c][uint3(phys)] = stored;
    occupancyCascades[c][uint3(phys)] = porous;

    // The finest cascade also splats the per-cell base color into the albedo cache. Alpha
    // carries the cell's matter: 1 where a solid brick covers it, the porous density where
    // only porous matter does, 0 for open space. The DDGI trace reads it at hit points and
    // inside dense aggregates.
    if (c == 0u)
    {
        float4 albedo;
        if (best < 1e29)
        {
            albedo = float4(bestAlbedo, 1.0);
        }
        else if (porous > 0.0)
        {
            albedo = float4(porousAlbedo, porous);
        }
        else
        {
            albedo = float4(0.0, 0.0, 0.0, 0.0);
        }
        albedoCache[uint3(phys)] = albedo;
    }
}
