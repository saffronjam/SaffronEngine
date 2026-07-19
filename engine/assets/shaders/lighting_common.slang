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
    public float4x4 shadowViewProj;
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

public float pointShadow(SamplerCube staticCube, SamplerCube dynCube, float3 lightPos, float3 worldPos)
{
    float3 toFrag = worldPos - lightPos;
    float dist = length(toFrag);
    float3 dir = normalize(toFrag);
    float stored = min(
        staticCube.SampleLevel(dir, 0.0).r,
        dynCube.SampleLevel(dir, 0.0).r);
    const float bias = 0.08;
    return dist - bias <= stored ? 1.0 : 0.0;
}

public float pcfShadow(Sampler2DShadow map, float4x4 viewProj, float3 worldPos)
{
    float4 clip = mul(viewProj, float4(worldPos, 1.0));
    float3 ndc = clip.xyz / clip.w;
    float2 uv = ndc.xy * 0.5 + 0.5;
    if (clip.w <= 0.0 || uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z > 1.0)
    {
        return 1.0;
    }
    const float texel = 1.0 / 2048.0;
    float sum = 0.0;
    for (int x = -1; x <= 1; x = x + 1)
    {
        for (int y = -1; y <= 1; y = y + 1)
        {
            sum += map.SampleCmpLevelZero(uv + float2(x, y) * texel, ndc.z);
        }
    }
    return sum / 9.0;
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
    LightGlobals lg, Sampler2DShadow spotMap, SamplerCube ptStatic, SamplerCube ptDyn)
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
            shadow = pcfShadow(spotMap, lg.spotShadowViewProj, worldPos);
        }
    }
    else if (castShadow && lg.pointShadowMeta.y != 0 && lightIndex == lg.pointShadowMeta.x)
    {
        shadow = pointShadow(ptStatic, ptDyn, lg.pointShadow.xyz, worldPos);
    }
    float phase = hgPhase(dot(viewDir, l), phaseG);
    return lt.colorIntensity.rgb * lt.colorIntensity.a * atten * cone * shadow * phase * volMult;
}

public float3 fogDirectionalInScatter(
    float3 worldPos, float3 viewDir, float phaseG, LightGlobals lg, Sampler2DShadow sunMap)
{
    float volMult = asfloat(lg.extraFlags.z);
    bool castShadow = lg.extraFlags.w != 0;
    float3 lDir = -normalize(lg.directionAmbient.xyz);
    float shadow = (castShadow && lg.counts.y != 0) ? pcfShadow(sunMap, lg.shadowViewProj, worldPos) : 1.0;
    float phase = hgPhase(dot(viewDir, lDir), phaseG);
    return lg.colorIntensity.rgb * lg.colorIntensity.a * shadow * phase * volMult;
}

public float3 fogAmbientInScatter(LightGlobals lg)
{
    return lg.ambientColor.rgb * max(lg.directionAmbient.w, 0.0) / (4.0 * LIGHTING_PI);
}
