// Cloud lighting shared by the adaptive view march: the Jendersie-d'Eon HG-Draine analytic
// Mie fit, cone-marched sun transmittance, live sky SH ambient, and two energy-conserving
// multiple-scattering octaves.

module cloud_lighting;

import clouds;
import sky_sh;

static const float CLOUD_PI = 3.14159265359;
static const float CLOUD_EXTINCTION = 0.002;
static const float CLOUD_ALBEDO = 0.999;

public float cloudHg(float u, float g)
{
    float g2 = g * g;
    return (1.0 - g2) /
        (4.0 * CLOUD_PI * pow(max(1.0 + g2 - 2.0 * g * u, 1e-5), 1.5));
}

public float cloudDraine(float u, float g, float alpha)
{
    float g2 = g * g;
    float hg = (1.0 - g2) /
        (4.0 * CLOUD_PI * pow(max(1.0 + g2 - 2.0 * g * u, 1e-5), 1.5));
    return hg * (1.0 + alpha * u * u) /
        (1.0 + alpha * (1.0 + 2.0 * g2) / 3.0);
}

public float cloudMiePhase(float u, float diameter, float eccentricityScale)
{
    float d = clamp(diameter, 5.0, 50.0);
    float gHg = exp(-0.0990567 / (d - 1.67154)) * eccentricityScale;
    float gD = exp(-2.20679 / (d + 3.91029) - 0.428934) * eccentricityScale;
    float alpha = exp(3.62489 - 8.29288 / (d + 5.52825));
    float wD = exp(-0.599085 / (d - 0.641583) - 0.665888);
    return lerp(cloudHg(u, gHg), cloudDraine(u, gD, alpha), wD);
}

public float3 cloudAtmosphereTransmittance(
    float3 worldPos,
    float3 sunDirection,
    CloudParams p,
    Sampler2D<float4> transmittanceLut)
{
    if (p.frame.w == 0u)
    {
        return float3(1.0);
    }
    float radius = p.atmosphere.x + max(worldPos.y, 0.0) * p.atmosphere.z;
    float mu = dot(float3(0.0, 1.0, 0.0), sunDirection);
    float2 uv = float2(mu * 0.5 + 0.5, saturate((radius - p.atmosphere.x) / p.atmosphere.y));
    return transmittanceLut.SampleLevel(uv, 0.0).rgb;
}

public float cloudSunTransmittance(
    float3 worldPos,
    float3 sunDirection,
    uint pixelHash,
    CloudParams p,
    Sampler3D<float4> baseNoise,
    Sampler3D<float4> detailNoise,
    Sampler2D<float4> curlNoise,
    Sampler2D<float4> weatherMap)
{
    float3 tangent = normalize(abs(sunDirection.y) < 0.95
        ? cross(sunDirection, float3(0.0, 1.0, 0.0))
        : cross(sunDirection, float3(1.0, 0.0, 0.0)));
    float3 bitangent = cross(sunDirection, tangent);
    uint steps = clamp(uint(p.march.y + 0.5), 1u, 32u);
    float opticalDepth = 0.0;
    for (uint i = 0u; i < steps; ++i)
    {
        float f = (float(i) + 0.5) / float(steps);
        float angle = 6.28318530718 * frac(float(pixelHash) * 0.61803398875 + f * 0.754877666);
        float cone = f * f * 0.16;
        float distance = lerp(80.0, 1800.0, f * f);
        if (i + 1u == steps)
        {
            distance *= 2.5;
        }
        float3 coneDirection = normalize(
            sunDirection + (cos(angle) * tangent + sin(angle) * bitangent) * cone);
        float density = sampleCloudDensity(
            worldPos + coneDirection * distance,
            p,
            baseNoise,
            detailNoise,
            curlNoise,
            weatherMap);
        opticalDepth += density * distance / float(steps);
    }
    return exp(-opticalDepth * CLOUD_EXTINCTION);
}

public float3 cloudIncidentLighting(
    float3 worldPos,
    float3 viewDirection,
    float heightFraction,
    uint pixelHash,
    CloudParams p,
    Sampler3D<float4> baseNoise,
    Sampler3D<float4> detailNoise,
    Sampler2D<float4> curlNoise,
    Sampler2D<float4> weatherMap,
    Sampler2D<float4> transmittanceLut,
    StructuredBuffer<float4> skySh,
    Sampler2DArray<float> cloudShadow)
{
    bool sunAbove = p.sunDirectionIntensity.y > 0.0 && p.sunDirectionIntensity.w > 0.0;
    float3 sunDirection = normalize(sunAbove
        ? p.sunDirectionIntensity.xyz
        : p.moonDirectionIntensity.xyz);
    float sunIntensity = sunAbove ? p.sunDirectionIntensity.w : p.moonDirectionIntensity.w;
    float3 sunColor = sunAbove ? p.sunColorDroplet.rgb : p.moonColorShadow.rgb;
    float sunT = p.moonColorShadow.w > 0.5
        ? cloudShadowVisibility(worldPos, cloudShadow, p, p.shadow.y)
        : cloudSunTransmittance(
            worldPos,
            sunDirection,
            pixelHash,
            p,
            baseNoise,
            detailNoise,
            curlNoise,
            weatherMap);
    float3 atmosphereT = cloudAtmosphereTransmittance(
        worldPos, sunDirection, p, transmittanceLut);
    float3 sunRadiance = sunColor * sunIntensity * atmosphereT;
    float cosTheta = dot(-viewDirection, sunDirection);

    float3 direct = float3(0.0);
    float scatterScale = 1.0;
    float extinctionScale = 1.0;
    float eccentricityScale = 1.0;
    [unroll]
    for (uint octave = 0u; octave < 2u; ++octave)
    {
        float phase = cloudMiePhase(
            cosTheta, p.sunColorDroplet.w, eccentricityScale);
        direct += scatterScale * pow(sunT, extinctionScale) * phase * sunRadiance;
        scatterScale *= 0.5;
        extinctionScale *= 0.5;
        eccentricityScale *= 0.5;
    }

    float3 ambientDirection = normalize(lerp(float3(0.0, -1.0, 0.0), float3(0.0, 1.0, 0.0),
        0.25 + 0.75 * saturate(heightFraction)));
    float groundBounce = lerp(0.22, 1.0, saturate(heightFraction));
    float3 ambient = skyShIrradiance(ambientDirection, skySh) * groundBounce / CLOUD_PI;
    return (direct + ambient) * CLOUD_ALBEDO;
}
