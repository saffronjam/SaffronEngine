// Shared volumetric-cloud shape primitives. The static field bakes use the periodic noise basis;
// both the density debugger and the production march call sampleCloudDensity for the complete
// weather-map -> height-envelope -> value-erosion shape path.

module clouds;

public struct CloudParams
{
    public float4x4 invViewProj;
    public float4x4 prevViewProj;
    public float4 cameraCoverage; // xyz camera position, w coverage
    public float4 layer;          // altitude, height, cloud type, precipitation
    public float4 shape;          // anvil, base scale, detail scale, detail strength
    public float4 weather;        // curl strength, weather scale, enabled, painted override
    public float4 weatherOffset;  // xyz offset, w reserved
    public float4 sunDirectionIntensity; // xyz direction to sun, w coupled intensity
    public float4 sunColorDroplet;       // rgb coupled color, w droplet diameter in micrometres
    public float4 march;                 // primary steps, light steps, fresh weight, history valid
    public uint4 frame;                  // jitter index, reduced width/height, atmosphere live
    public float4 atmosphere;            // planet radius km, height km, metres->km, reserved
    public float4 moonDirectionIntensity; // xyz direction to moon, w coupled intensity
    public float4 moonColorShadow;        // rgb coupled color, w cloud-shadow enabled
    public float4 windTime;               // xy horizontal direction, z speed, w time seconds
    public float4 shadow;                 // x gust, y cloud/fog strength, z surface strength
    public float4 shadowRight;
    public float4 shadowUp;
    public float4 shadowCenters[3];        // xyz snapped center, w half extent
    public float4 shadowLight;             // xyz direction to key light, w ESM exponent
};

public float cloudShadowVisibility(
    float3 worldPos,
    Sampler2DArray<float> cloudShadow,
    CloudParams p,
    float strength)
{
    float2 projected = float2(0.0);
    uint cascade = 2u;
    bool cascadeFound = false;
    for (uint i = 0u; i < 3u; ++i)
    {
        float3 delta = worldPos - p.shadowCenters[i].xyz;
        float extent = p.shadowCenters[i].w;
        projected = float2(dot(delta, p.shadowRight.xyz), dot(delta, p.shadowUp.xyz)) / extent;
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
    float encoded = cloudShadow.SampleLevel(float3(projected * 0.5 + 0.5, float(cascade)), 0.0);
    float beer = saturate(log(max(encoded, 1.0)) / max(p.shadowLight.w, 1e-4));
    return lerp(1.0, beer, saturate(strength));
}

public float cloudRemap(float v, float o0, float o1, float n0, float n1)
{
    return n0 + saturate((v - o0) / max(o1 - o0, 1e-5)) * (n1 - n0);
}

public uint cloudHash(uint3 p)
{
    uint h = p.x * 1597334677u ^ p.y * 3812015801u ^ p.z * 2798796415u;
    h = (h ^ (h >> 16u)) * 2246822519u;
    return h ^ (h >> 13u);
}

public uint3 cloudWrapCell(int3 p, uint period)
{
    int ip = int(period);
    return uint3((p % ip + ip) % ip);
}

public float3 cloudGradient(uint3 cell)
{
    uint h = cloudHash(cell);
    float3 g = float3(
        float(int(h & 1023u) - 511),
        float(int((h >> 10u) & 1023u) - 511),
        float(int((h >> 20u) & 1023u) - 511));
    return normalize(g + float3(1e-3, 0.0, 0.0));
}

public float periodicPerlin(float3 p, uint period)
{
    int3 cell = int3(floor(p));
    float3 f = frac(p);
    float3 u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    float values[8];
    for (uint i = 0u; i < 8u; ++i)
    {
        int3 corner = int3(int(i & 1u), int((i >> 1u) & 1u), int((i >> 2u) & 1u));
        float3 delta = f - float3(corner);
        values[i] = dot(cloudGradient(cloudWrapCell(cell + corner, period)), delta);
    }
    float x00 = lerp(values[0], values[1], u.x);
    float x10 = lerp(values[2], values[3], u.x);
    float x01 = lerp(values[4], values[5], u.x);
    float x11 = lerp(values[6], values[7], u.x);
    return lerp(lerp(x00, x10, u.y), lerp(x01, x11, u.y), u.z);
}

public float periodicWorley(float3 p, uint period)
{
    int3 cell = int3(floor(p));
    float best = 8.0;
    for (int z = -1; z <= 1; ++z)
    {
        for (int y = -1; y <= 1; ++y)
        {
            for (int x = -1; x <= 1; ++x)
            {
                int3 neighbor = cell + int3(x, y, z);
                uint3 wrapped = cloudWrapCell(neighbor, period);
                uint h = cloudHash(wrapped);
                float3 feature = float3(
                    float(h & 1023u),
                    float((h >> 10u) & 1023u),
                    float((h >> 20u) & 1023u)) / 1023.0;
                float3 delta = float3(neighbor) + feature - p;
                best = min(best, dot(delta, delta));
            }
        }
    }
    return saturate(sqrt(best) * 0.75);
}

public float cloudHeightGradient(float h, float type)
{
    float stratus = cloudRemap(h, 0.0, 0.10, 0.0, 1.0)
                  * cloudRemap(h, 0.20, 0.30, 1.0, 0.0);
    float cumulus = cloudRemap(h, 0.0, 0.25, 0.0, 1.0)
                   * cloudRemap(h, 0.60, 0.95, 1.0, 0.0);
    float cumulonimbus = cloudRemap(h, 0.0, 0.10, 0.0, 1.0)
                        * cloudRemap(h, 0.80, 1.00, 1.0, 0.0);
    float a = saturate(type * 2.0);
    float b = saturate(type * 2.0 - 1.0);
    return lerp(lerp(stratus, cumulus, a), cumulonimbus, b);
}

public float cloudPrecipitationDensity(float density, float precipitation)
{
    return saturate(density * lerp(1.0, 1.5, saturate(precipitation)));
}

public float sampleCloudDensity(
    float3 worldPos,
    CloudParams p,
    Sampler3D<float4> baseNoise,
    Sampler3D<float4> detailNoise,
    Sampler2D<float4> curlNoise,
    Sampler2D<float4> weatherMap)
{
    float h = (worldPos.y - p.layer.x) / p.layer.y;
    if (h < 0.0 || h > 1.0 || p.weather.z < 0.5)
    {
        return 0.0;
    }

    float2 windOffset = p.windTime.xy * p.windTime.z * p.windTime.w;
    float2 weatherUv = (worldPos.xz - windOffset) * p.weather.y + p.weatherOffset.xz;
    float3 wm = weatherMap.SampleLevel(weatherUv, 0.0).rgb;
    float coverage = saturate(wm.r * p.cameraCoverage.w);
    float type = saturate(wm.b + p.layer.z);
    float profile = cloudHeightGradient(h, type) * coverage;
    float anvilPower = cloudRemap(h, 0.7, 0.8, 1.0, lerp(1.0, 0.5, p.shape.x));
    profile = pow(saturate(profile), anvilPower);

    float3 advectedPos = worldPos - float3(windOffset.x, 0.0, windOffset.y);
    float4 base = baseNoise.SampleLevel(advectedPos * p.shape.y, 0.0);
    float shapeThreshold = dot(base.gba, float3(0.625, 0.25, 0.125)) - 1.0;
    float billow = cloudRemap(base.r, shapeThreshold, 1.0, 0.0, 1.0);
    float density = saturate(billow - (1.0 - profile));

    float2 curlUv = advectedPos.xz * p.shape.y;
    float potentialXp = curlNoise.SampleLevel(curlUv + float2(1.0 / 128.0, 0.0), 0.0).r;
    float potentialXm = curlNoise.SampleLevel(curlUv - float2(1.0 / 128.0, 0.0), 0.0).r;
    float potentialYp = curlNoise.SampleLevel(curlUv + float2(0.0, 1.0 / 128.0), 0.0).r;
    float potentialYm = curlNoise.SampleLevel(curlUv - float2(0.0, 1.0 / 128.0), 0.0).r;
    float2 curl = float2(potentialYp - potentialYm, potentialXm - potentialXp) * p.shadow.x;
    float3 detailPos = advectedPos + float3(curl.x, 0.0, curl.y) * p.weather.x;
    float3 detail = detailNoise.SampleLevel(detailPos * p.shape.z, 0.0).rgb;
    float erosion = dot(detail, float3(0.625, 0.25, 0.125)) * p.shape.w;
    float eroded = cloudRemap(density, erosion * (1.0 - density), 1.0, 0.0, 1.0);
    return cloudPrecipitationDensity(eroded, wm.g);
}

public float sampleCloudConservativeDensity(
    float3 worldPos,
    CloudParams p,
    Sampler3D<float4> baseNoise,
    Sampler2D<float4> weatherMap)
{
    float h = (worldPos.y - p.layer.x) / p.layer.y;
    if (h < 0.0 || h > 1.0 || p.weather.z < 0.5)
    {
        return 0.0;
    }
    float2 windOffset = p.windTime.xy * p.windTime.z * p.windTime.w;
    float2 weatherUv = (worldPos.xz - windOffset) * p.weather.y + p.weatherOffset.xz;
    float3 wm = weatherMap.SampleLevel(weatherUv, 0.0).rgb;
    float coverage = saturate(wm.r * p.cameraCoverage.w);
    float type = saturate(wm.b + p.layer.z);
    float profile = cloudHeightGradient(h, type) * coverage;
    float anvilPower = cloudRemap(h, 0.7, 0.8, 1.0, lerp(1.0, 0.5, p.shape.x));
    profile = pow(saturate(profile), anvilPower);
    float3 advectedPos = worldPos - float3(windOffset.x, 0.0, windOffset.y);
    float4 base = baseNoise.SampleLevel(advectedPos * p.shape.y, 0.0);
    float threshold = dot(base.gba, float3(0.625, 0.25, 0.125)) - 1.0;
    float billow = cloudRemap(base.r, threshold, 1.0, 0.0, 1.0);
    float density = saturate(billow - (1.0 - profile));
    return cloudPrecipitationDensity(density, wm.g);
}
