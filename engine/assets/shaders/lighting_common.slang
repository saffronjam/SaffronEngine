// Resource-free lighting data and sampling helpers shared by forward surfaces and volumetric fog.

module lighting_common;

static const uint MAX_LIGHTS_PER_CLUSTER = 64;
static const float LIGHTING_PI = 3.14159265;

public struct LightGlobals
{
    public float4 directionAmbient;
    public float4 colorIntensity;
    public float4 moonDirectionIntensity;
    public float4 moonColor;
    public uint4 counts;
    public float4 eyePosition;
    public float4x4 spotShadowViewProj;
    public uint4 spotShadow;
    public float4 pointShadow;
    public uint4 pointShadowMeta;
    public uint4 screenFlags;
    public float4 ddgiVolumeMin;
    public float4 ddgiVolumeExtent;
    public uint4 ddgiProbeCount;
    public uint4 ddgiScrollBase;
    public uint4 sdfOcclusion;
    public float4 ambientColor;
    public float4 iblTint;
    public uint4 extraFlags;
    public float4x4 prevViewProj;
    public float4 froxelFog;
    public float4 cloudShadowRight;
    public float4 cloudShadowUp;
    public float4 cloudShadowCenters[3];
    public float4 cloudShadowMeta;
    // Shared wind field: xy = mean direction (x, z), z = speed m/s, w = gust fraction.
    public float4 windDirSpeedGust;
    // x = turbulence roughness, y = gust frequency Hz, z = reference height m,
    // w = height shear exponent.
    public float4 windParams;
    // x = turbulence octaves, y = phase seed.
    public uint4 windMeta;
    // x = simulation seconds this frame, y = previous frame's seconds.
    public float4 windTime;
    // The frame's local wind-source list (device address + reserved pad); the
    // count rides windMeta.z.
    public uint64_t windSources;
    public uint64_t windSourcesReserved;
    // The directional virtual shadow space: world -> light rotation, per-level
    // snapped windows (origin.xy, extent), depth centre/half-span/enable, and the
    // frame's page-table address (8 levels x 1024 packed entries).
    public float4x4 vsmBasis;
    public float4 vsmLevels[8];
    public float4 vsmParams;
    public uint64_t vsmPageTable;
    public uint64_t vsmReserved;
};

public struct GpuLight
{
    public float4 positionRange;
    public float4 colorIntensity;
    public float4 directionType;
    public float4 spotCos;
};

public struct Cluster
{
    public uint count;
    public uint indices[MAX_LIGHTS_PER_CLUSTER];
};

public struct ClusterParams
{
    public float4x4 view;
    public float4x4 inverseProjection;
    public uint4 gridSize;
    public uint4 screenSize;
    public float4 zPlanes;
};

public float cloudShadowVisibility(
    float3 worldPos,
    Sampler2DArray<float> map,
    LightGlobals lg,
    float strength)
{
    if (lg.cloudShadowMeta.w < 0.5)
    {
        return 1.0;
    }
    float2 projected = float2(0.0);
    uint cascade = 2u;
    bool cascadeFound = false;
    for (uint i = 0u; i < 3u; ++i)
    {
        float3 delta = worldPos - lg.cloudShadowCenters[i].xyz;
        float extent = lg.cloudShadowCenters[i].w;
        projected = float2(dot(delta, lg.cloudShadowRight.xyz), dot(delta, lg.cloudShadowUp.xyz))
                  / extent;
        if (all(abs(projected) <= 1.0))
        {
            cascade = i;
            cascadeFound = true;
            break;
        }
    }
    if (!cascadeFound)
    {
        return 1.0;
    }
    float encoded = map.SampleLevel(float3(projected * 0.5 + 0.5, float(cascade)), 0.0);
    float beer = saturate(log(max(encoded, 1.0)) / max(lg.cloudShadowMeta.x, 1e-4));
    return lerp(1.0, beer, saturate(strength));
}

// The packed virtual-shadow page-table entry: bit 31 = resident, low 10 bits =
// the physical atlas tile.
public static const uint VSM_TABLE_RESIDENT = 1u << 31u;
static const float VSM_ATLAS_TEXELS = 4096.0;
static const float VSM_PAGE_TEXELS = 128.0;
static const uint VSM_LEVEL_PAGES = 32u;

// Samples the directional virtual shadow map: rotate into the light plane, walk
// the clip levels fine to coarse for the first level whose window contains the
// receiver AND whose page is resident, then 3x3 PCF inside the page's atlas tile
// (taps clamped one texel inside the tile so filtering never crosses pages). A
// receiver with no resident page anywhere reads unshadowed — the coarser walk is
// the defined fallback, never a leak from foreign content.
public float vsmSampleDirectional(float3 worldPos, LightGlobals lg, Sampler2DShadow atlas)
{
    if (lg.vsmPageTable == 0 || lg.vsmParams.z == 0.0)
    {
        return 1.0;
    }
    float3 lightPos = mul(lg.vsmBasis, float4(worldPos, 1.0)).xyz;
    float depth = (lightPos.z - (lg.vsmParams.x - lg.vsmParams.y)) / (2.0 * lg.vsmParams.y);
    if (depth <= 0.0 || depth >= 1.0)
    {
        return 1.0;
    }
    for (uint level = 0u; level < 8u; ++level)
    {
        float4 window = lg.vsmLevels[level];
        if (window.z <= 0.0)
        {
            continue;
        }
        float2 local = (lightPos.xy - window.xy) / window.z;
        if (local.x < 0.0 || local.y < 0.0 || local.x >= 1.0 || local.y >= 1.0)
        {
            continue;
        }
        float2 scaled = local * float(VSM_LEVEL_PAGES);
        uint2 page = uint2(scaled);
        uint entry = *(uint*)(
            lg.vsmPageTable
            + (uint64_t(level) * (VSM_LEVEL_PAGES * VSM_LEVEL_PAGES)
               + page.y * VSM_LEVEL_PAGES + page.x) * 4u);
        if ((entry & VSM_TABLE_RESIDENT) == 0u)
        {
            continue;
        }
        return vsmTilePcf(atlas, entry, scaled - float2(page), depth);
    }
    return 1.0;
}

// 3x3 PCF inside one resident page's atlas tile; `pageFrac` is the receiver's
// fractional position within the page and taps clamp one texel inside the tile so
// filtering never crosses pages.
float vsmTilePcf(Sampler2DShadow atlas, uint entry, float2 pageFrac, float depth)
{
    uint tile = entry & 0x3ffu;
    float2 texel = clamp(pageFrac * VSM_PAGE_TEXELS, 1.5, VSM_PAGE_TEXELS - 1.5);
    float2 tileBase = float2(tile % VSM_LEVEL_PAGES, tile / VSM_LEVEL_PAGES) * VSM_PAGE_TEXELS;
    float2 baseUv = (tileBase + texel) / VSM_ATLAS_TEXELS;
    float sum = 0.0;
    for (int dy = -1; dy <= 1; ++dy)
    {
        for (int dx = -1; dx <= 1; ++dx)
        {
            sum += atlas.SampleCmpLevelZero(baseUv + float2(dx, dy) / VSM_ATLAS_TEXELS, depth);
        }
    }
    return sum / 9.0;
}

// Samples the shadowed spot's virtual pages: project by the spot transform, look
// the page up in the table's spot region (base 8192), and PCF inside its tile. A
// missing page reads unshadowed — the spot has no coarser level to fall to.
public float vsmSampleSpot(float3 worldPos, LightGlobals lg, Sampler2DShadow atlas)
{
    if (lg.vsmPageTable == 0 || lg.vsmParams.z == 0.0)
    {
        return 1.0;
    }
    float4 clip = mul(lg.spotShadowViewProj, float4(worldPos, 1.0));
    if (clip.w <= 0.0)
    {
        return 1.0;
    }
    float3 ndc = clip.xyz / clip.w;
    if (abs(ndc.x) >= 1.0 || abs(ndc.y) >= 1.0 || ndc.z <= 0.0 || ndc.z >= 1.0)
    {
        return 1.0;
    }
    float2 uv = ndc.xy * 0.5 + 0.5;
    float2 scaled = uv * 16.0;
    uint2 page = uint2(scaled);
    uint entry = *(uint*)(lg.vsmPageTable + uint64_t(8192u + page.y * 16u + page.x) * 4u);
    if ((entry & VSM_TABLE_RESIDENT) == 0u)
    {
        return 1.0;
    }
    return vsmTilePcf(atlas, entry, scaled - float2(page), ndc.z);
}

// Samples the shadowed point light's virtual pages: pick the cube face by the
// dominant axis of the light->fragment direction (the standard cube major-axis
// mapping, matching the face matrices the page renderer rasterizes with), look
// the page up in the table's point region (base 8448, 64 entries per face), and
// PCF inside its tile. A missing page reads unshadowed.
public float vsmSamplePoint(float3 worldPos, LightGlobals lg, Sampler2DShadow atlas)
{
    if (lg.vsmPageTable == 0 || lg.vsmParams.z == 0.0)
    {
        return 1.0;
    }
    float3 toFrag = worldPos - lg.pointShadow.xyz;
    float far = lg.pointShadow.w;
    float3 a = abs(toFrag);
    uint face;
    float axis;
    float2 st;
    if (a.x >= a.y && a.x >= a.z)
    {
        face = toFrag.x > 0.0 ? 0u : 1u;
        axis = a.x;
        st = float2(toFrag.x > 0.0 ? -toFrag.z : toFrag.z, -toFrag.y);
    }
    else if (a.y >= a.z)
    {
        face = toFrag.y > 0.0 ? 2u : 3u;
        axis = a.y;
        st = float2(toFrag.x, toFrag.y > 0.0 ? toFrag.z : -toFrag.z);
    }
    else
    {
        face = toFrag.z > 0.0 ? 4u : 5u;
        axis = a.z;
        st = float2(toFrag.z > 0.0 ? toFrag.x : -toFrag.x, -toFrag.y);
    }
    // The face frustum's near plane and 0..1 depth mapping mirror the page
    // renderer's 90-degree projection (near 0.05, far = the light range).
    const float near = 0.05;
    if (axis <= near || axis >= far)
    {
        return 1.0;
    }
    float depth = far * (axis - near) / (axis * (far - near));
    float receiverBiasM = 0.03;
    float depthBias = (far * near / (far - near)) * receiverBiasM / max(axis * axis, 1e-4);
    depth = max(depth - depthBias, 0.0);
    float2 scaled = clamp(st / axis * 0.5 + 0.5, 0.0, 0.999) * 8.0;
    uint2 page = uint2(scaled);
    uint entry = *(uint*)(
        lg.vsmPageTable + uint64_t(8448u + face * 64u + page.y * 8u + page.x) * 4u);
    if ((entry & VSM_TABLE_RESIDENT) == 0u)
    {
        return 1.0;
    }
    return vsmTilePcf(atlas, entry, scaled - float2(page), depth);
}

public float distanceAttenuation(float dist, float range)
{
    float invSquare = 1.0 / max(dist * dist, 0.0001);
    float t = saturate(1.0 - pow(dist / range, 4.0));
    return invSquare * t * t;
}

public float hgPhase(float cosTheta, float g)
{
    float g2 = g * g;
    float denom = 1.0 + g2 - 2.0 * g * cosTheta;
    return (1.0 - g2) / (4.0 * LIGHTING_PI * pow(max(denom, 1e-4), 1.5));
}

public uint clusterIndexFor(ClusterParams p, float2 pixel, float viewZ)
{
    float near = p.zPlanes.x;
    float far = p.zPlanes.y;
    float depth = max(-viewZ, near);
    uint zSlice = uint(log(depth / near) / log(far / near) * float(p.gridSize.z));
    zSlice = min(zSlice, p.gridSize.z - 1);

    float2 tileSize = float2(p.screenSize.xy) / float2(p.gridSize.xy);
    uint tileX = min(uint(pixel.x / tileSize.x), p.gridSize.x - 1);
    uint tileY = min(uint(pixel.y / tileSize.y), p.gridSize.y - 1);
    return tileX + tileY * p.gridSize.x + zSlice * p.gridSize.x * p.gridSize.y;
}

public float3 fogPunctualInScatter(
    GpuLight lt, uint lightIndex, float3 worldPos, float3 viewDir, float phaseG,
    LightGlobals lg, Sampler2DShadow atlas)
{
    float3 toLight = lt.positionRange.xyz - worldPos;
    float dist = length(toLight);
    if (dist > lt.positionRange.w)
    {
        return float3(0.0);
    }
    float3 l = toLight / max(dist, 0.0001);
    float atten = distanceAttenuation(dist, lt.positionRange.w);
    float cone = 1.0;
    float shadow = 1.0;
    float volMult = lt.spotCos.z;
    bool castShadow = lt.spotCos.w >= 0.5;
    if (lt.directionType.w > 0.5)
    {
        float3 spotDir = normalize(lt.directionType.xyz);
        cone = smoothstep(lt.spotCos.y, lt.spotCos.x, dot(spotDir, -l));
        if (castShadow && lg.spotShadow.y != 0 && lightIndex == lg.spotShadow.x)
        {
            shadow = vsmSampleSpot(worldPos, lg, atlas);
        }
    }
    else if (castShadow && lg.pointShadowMeta.y != 0 && lightIndex == lg.pointShadowMeta.x)
    {
        shadow = vsmSamplePoint(worldPos, lg, atlas);
    }
    float phase = hgPhase(dot(viewDir, l), phaseG);
    return lt.colorIntensity.rgb * lt.colorIntensity.a * atten * cone * shadow * phase * volMult;
}

public float3 fogDirectionalInScatter(
    float3 worldPos, float3 viewDir, float phaseG, LightGlobals lg, Sampler2DShadow atlas)
{
    float volMult = asfloat(lg.extraFlags.z);
    bool castShadow = lg.extraFlags.w != 0;
    float3 lDir = -normalize(lg.directionAmbient.xyz);
    float shadow = (castShadow && lg.counts.y != 0) ? vsmSampleDirectional(worldPos, lg, atlas) : 1.0;
    float phase = hgPhase(dot(viewDir, lDir), phaseG);
    return lg.colorIntensity.rgb * lg.colorIntensity.a * shadow * phase * volMult;
}

public float3 fogAmbientInScatter(LightGlobals lg)
{
    return lg.ambientColor.rgb * max(lg.directionAmbient.w, 0.0) / (4.0 * LIGHTING_PI);
}
