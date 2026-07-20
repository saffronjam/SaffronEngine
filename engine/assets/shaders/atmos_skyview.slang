// Sky-view LUT (Hillaire 2020): for each texel (azimuth, elevation) ray-marches the in-scattered
// sky radiance from the camera altitude, applying Rayleigh + HG-Mie phase, transmittance, and the
// multiple-scattering term. Elevation is horizon-densified (more resolution near the horizon).
// Set 0 binding 0 = transmittance, binding 1 = multiscatter, binding 2 = rgba16f storage output.

[[vk::binding(0, 0)]] Sampler2D<float4> transmittanceLut;
[[vk::binding(1, 0)]] Sampler2D<float4> multiScatterLut;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 0)]] RWTexture2D<float4> outLut;

struct AtmosParams
{
    float4 sunDir;    // xyz = dir to sun, w = sun intensity
    float4 rayleigh;
    float4 ozone;
    float4 params0;   // planetRadius, atmosphereHeight, mieScaleHeight, mieAnisotropy
    float4 params1;   // sunDiskAngularRadius, sunDiskIntensity, cameraAltitude, unused
    float4 moon;
    float4 moonDisk;
};
[[vk::push_constant]] AtmosParams pc;

static const float PI = 3.14159265;

float planetRadius() { return pc.params0.x; }
float atmosTop() { return pc.params0.x + pc.params0.y; }

void densities(float h, out float rayleigh, out float mie, out float ozone)
{
    rayleigh = exp(-max(h, 0.0) / pc.rayleigh.w);
    mie = exp(-max(h, 0.0) / pc.params0.z);
    ozone = max(0.0, 1.0 - abs(h - 25.0) / 15.0);
}

float3 sampleTransmittance(float r, float mu)
{
    float u = mu * 0.5 + 0.5;
    float v = saturate((r - planetRadius()) / pc.params0.y);
    return transmittanceLut.SampleLevel(float2(u, v), 0.0).rgb;
}

float3 sampleMultiScatter(float r, float sunMu)
{
    float u = sunMu * 0.5 + 0.5;
    float v = saturate((r - planetRadius()) / pc.params0.y);
    return multiScatterLut.SampleLevel(float2(u, v), 0.0).rgb;
}

float rayleighPhase(float cosTheta)
{
    return (3.0 / (16.0 * PI)) * (1.0 + cosTheta * cosTheta);
}

float hgPhase(float cosTheta, float g)
{
    float g2 = g * g;
    float denom = 1.0 + g2 - 2.0 * g * cosTheta;
    return (1.0 - g2) / (4.0 * PI * pow(max(denom, 1e-4), 1.5));
}

float rayTopDistance(float r, float mu)
{
    float top = atmosTop();
    float disc = r * r * (mu * mu - 1.0) + top * top;
    return max(0.0, -r * mu + sqrt(max(disc, 0.0)));
}

bool hitsGround(float r, float mu, out float groundDist)
{
    float rg = planetRadius();
    float disc = r * r * (mu * mu - 1.0) + rg * rg;
    groundDist = 0.0;
    if (mu < 0.0 && disc >= 0.0)
    {
        groundDist = max(0.0, -r * mu - sqrt(max(disc, 0.0)));
        return true;
    }
    return false;
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    outLut.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }

    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height);
    // Azimuth around the sun, elevation horizon-densified (Hillaire's sqrt mapping about y=0.5).
    float azimuth = uv.x * 2.0 * PI;
    float vCentered = uv.y - 0.5;
    float elevation = (vCentered >= 0.0 ? 1.0 : -1.0) * (vCentered * vCentered) * 2.0 * (PI * 0.5);

    float cosEl = cos(elevation);
    float3 viewDir = float3(cosEl * cos(azimuth), sin(elevation), cosEl * sin(azimuth));
    float3 sunDir = normalize(pc.sunDir.xyz);

    float r = planetRadius() + max(pc.params1.z, 0.5);  // camera altitude (km), min 0.5
    float3 pos = float3(0.0, r, 0.0);
    float mu = viewDir.y;

    float dist = rayTopDistance(r, mu);
    float groundDist;
    if (hitsGround(r, mu, groundDist))
    {
        dist = groundDist;
    }

    float cosThetaSun = dot(viewDir, sunDir);
    float phaseR = rayleighPhase(cosThetaSun);
    float phaseM = hgPhase(cosThetaSun, pc.params0.w);

    const int steps = 32;
    float dt = dist / float(steps);
    float3 transmittance = float3(1.0, 1.0, 1.0);
    float3 radiance = float3(0.0, 0.0, 0.0);
    float3 sunColor = pc.sunDir.w.xxx;  // intensity carried in sunDir.w; tint applied at skygen

    for (int k = 0; k < steps; ++k)
    {
        float t = (float(k) + 0.5) * dt;
        float3 sp = pos + viewDir * t;
        float sr = max(length(sp), planetRadius());
        float h = sr - planetRadius();
        float rayD;
        float mieD;
        float ozoneD;
        densities(h, rayD, mieD, ozoneD);

        float3 rayleighS = pc.rayleigh.xyz * rayD;
        float3 mieS = (pc.ozone.w * mieD).xxx;
        float3 ext = rayleighS + mieS + pc.ozone.xyz * ozoneD + pc.ozone.w * 0.1 * mieD;
        float3 stepT = exp(-ext * dt * 1e-3);

        float sunMu = dot(normalize(sp), sunDir);
        float3 sunT = sampleTransmittance(sr, sunMu);
        float3 ms = sampleMultiScatter(sr, sunMu);

        // Single scattering uses the directional phase; multi-scatter is isotropic.
        float3 inScatterSingle = (rayleighS * phaseR + mieS * phaseM) * sunT;
        float3 inScatterMulti = (rayleighS + mieS) * ms;
        float3 inScatter = (inScatterSingle + inScatterMulti) * sunColor;

        // Analytic step integral ∫ inScatter·T dt = inScatter·(1 - stepT)/ext. The
        // inScatter/ext ratio is a unit-independent scattering albedo; the km<->Mm
        // conversion already lives in stepT's optical depth, so divide by the raw ext.
        // An extra 1e-3 here over-brightens the sky 1000x (saturates the scene white).
        float3 sInt = (inScatter - inScatter * stepT) / max(ext, float3(1e-6, 1e-6, 1e-6));
        radiance += transmittance * sInt;
        transmittance *= stepT;
    }

    outLut[tid.xy] = float4(radiance, 1.0);
}
