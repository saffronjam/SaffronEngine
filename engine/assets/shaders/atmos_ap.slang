// One bounded atmosphere LUT march shared by aerial-perspective volume fill and the cloud ledger.

module atmos_ap;

static const float ATMOS_AP_PI = 3.14159265;

public struct ApParams
{
    public float4x4 invProjection;
    public float4x4 invView;
    public float4 sunDir;
    public float4 rayleigh;
    public float4 ozone;
    public float4 params0;
    public float4 params1;
    public float4 apPlanes;
};

public struct AtmoSample
{
    public float3 inScatter;
    public float3 transmittance;
};

float atmospherePlanetRadius(ApParams p)
{
    return p.params0.x;
}

void atmosphereDensities(ApParams p, float h, out float rayleigh, out float mie, out float ozone)
{
    rayleigh = exp(-max(h, 0.0) / p.rayleigh.w);
    mie = exp(-max(h, 0.0) / p.params0.z);
    ozone = max(0.0, 1.0 - abs(h - 25.0) / 15.0);
}

float3 atmosphereTransmittance(
    ApParams p, Sampler2D<float4> transmittanceLut, float r, float mu)
{
    float2 uv = float2(mu * 0.5 + 0.5,
                       saturate((r - atmospherePlanetRadius(p)) / p.params0.y));
    return transmittanceLut.SampleLevel(uv, 0.0).rgb;
}

float3 atmosphereMultiScatter(
    ApParams p, Sampler2D<float4> multiScatterLut, float r, float sunMu)
{
    float2 uv = float2(sunMu * 0.5 + 0.5,
                       saturate((r - atmospherePlanetRadius(p)) / p.params0.y));
    return multiScatterLut.SampleLevel(uv, 0.0).rgb;
}

float atmosphereRayleighPhase(float cosTheta)
{
    return (3.0 / (16.0 * ATMOS_AP_PI)) * (1.0 + cosTheta * cosTheta);
}

float atmosphereHgPhase(float cosTheta, float g)
{
    float g2 = g * g;
    float denom = 1.0 + g2 - 2.0 * g * cosTheta;
    return (1.0 - g2) / (4.0 * ATMOS_AP_PI * pow(max(denom, 1e-4), 1.5));
}

public float3 atmosphereObserverPosition(ApParams p)
{
    return float3(0.0, atmospherePlanetRadius(p) + max(p.params1.z, 0.5), 0.0);
}

public AtmoSample marchAtmosphere(
    float3 pos,
    float3 viewDir,
    float3 sunDir,
    float distKm,
    Sampler2D<float4> transmittanceLut,
    Sampler2D<float4> multiScatterLut,
    ApParams p)
{
    float cosThetaSun = dot(viewDir, sunDir);
    float phaseR = atmosphereRayleighPhase(cosThetaSun);
    float phaseM = atmosphereHgPhase(cosThetaSun, p.params0.w);
    float3 sunColor = p.sunDir.w.xxx;
    const int steps = 16;
    float dt = max(distKm, 0.0) / float(steps);
    float3 transmittance = float3(1.0);
    float3 radiance = float3(0.0);

    for (int k = 0; k < steps; ++k)
    {
        float t = (float(k) + 0.5) * dt;
        float3 sp = pos + viewDir * t;
        float sr = max(length(sp), atmospherePlanetRadius(p));
        float h = sr - atmospherePlanetRadius(p);
        float rayD;
        float mieD;
        float ozoneD;
        atmosphereDensities(p, h, rayD, mieD, ozoneD);

        float3 rayleighS = p.rayleigh.xyz * rayD;
        float3 mieS = (p.ozone.w * mieD).xxx;
        float3 ext = rayleighS + mieS + p.ozone.xyz * ozoneD + p.ozone.w * 0.1 * mieD;
        float3 stepT = exp(-ext * dt * 1e-3);

        float sunMu = dot(normalize(sp), sunDir);
        float3 sunT = atmosphereTransmittance(p, transmittanceLut, sr, sunMu);
        float3 ms = atmosphereMultiScatter(p, multiScatterLut, sr, sunMu);
        float3 singleScatter = (rayleighS * phaseR + mieS * phaseM) * sunT;
        float3 multipleScatter = (rayleighS + mieS) * ms;
        float3 source = (singleScatter + multipleScatter) * sunColor;
        float3 integrated = (source - source * stepT) / max(ext, float3(1e-6));
        radiance += transmittance * integrated;
        transmittance *= stepT;
    }

    AtmoSample sample;
    sample.inScatter = radiance;
    sample.transmittance = transmittance;
    return sample;
}
