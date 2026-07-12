// Aerial perspective (Hillaire 2020): fills a 32³ froxel volume with the atmosphere in-scatter +
// mean transmittance out to each froxel center's distance, ray-marching the same transmittance +
// multiscatter LUTs the sky-view LUT bakes (`atmos_skyview.slang`). One thread per froxel: reconstruct
// the froxel-center world position from the inverse view-proj + the exponential-Z slice, march from the
// camera to that point sampling the LUTs (bounded at the froxel distance instead of the atmosphere
// edge), and write `(inScatter, meanTransmittance)`. This is `atmos_skyview.slang` minus the background
// framing plus a depth-bounded stop — the only construction that yields the correct range-dependent
// blue-shift; the sky-view LUT itself is distance-free and cannot tint geometry.
//
// set 0: binding 0 = transmittance LUT, binding 1 = multiscatter LUT (both combined image samplers,
// the shared IBL clamp sampler), binding 2 = the AP volume (rgba16f storage image, GENERAL), binding 3
// = the ApParams UBO. The render graph transitions the volume to GENERAL before the dispatch.

[[vk::binding(0, 0)]] Sampler2D<float4> transmittanceLut;
[[vk::binding(1, 0)]] Sampler2D<float4> multiScatterLut;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 0)]] RWTexture3D<float4> apVolume;

struct ApParams
{
    float4x4 invProjection; // clip -> view (froxel-center reconstruction)
    float4x4 invView;       // view -> world (froxel-center reconstruction)
    float4 sunDir;          // xyz dir to sun, w sun intensity
    float4 rayleigh;        // xyz rayleigh scattering, w rayleigh scale height (km)
    float4 ozone;           // xyz ozone absorption, w mie scattering
    float4 params0;         // planetRadius, atmosphereHeight, mieScaleHeight, mieAnisotropy (km / -)
    float4 params1;         // sunDiskAngularRadius, sunDiskIntensity, cameraAltitude(km), apIntensity
    float4 apPlanes;        // x near (m), y far (m), z grid edge, w metres->km scale (1e-3)
};
[[vk::binding(3, 0)]] ConstantBuffer<ApParams> params;

static const float PI = 3.14159265;

float planetRadius() { return params.params0.x; }

void densities(float h, out float rayleigh, out float mie, out float ozone)
{
    rayleigh = exp(-max(h, 0.0) / params.rayleigh.w);
    mie = exp(-max(h, 0.0) / params.params0.z);
    ozone = max(0.0, 1.0 - abs(h - 25.0) / 15.0);
}

float3 sampleTransmittance(float r, float mu)
{
    float u = mu * 0.5 + 0.5;
    float v = saturate((r - planetRadius()) / params.params0.y);
    return transmittanceLut.SampleLevel(float2(u, v), 0.0).rgb;
}

float3 sampleMultiScatter(float r, float sunMu)
{
    float u = sunMu * 0.5 + 0.5;
    float v = saturate((r - planetRadius()) / params.params0.y);
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

// The froxel-center world position: the AP exponential-Z view distance at the slice center,
// unprojected through the tile center and moved to world space (the inverse of the composite's AP W
// mapping). No sub-froxel jitter — the LUT march is frame-stable, so the AP volume carries no history.
float3 froxelCenterWorld(uint3 id, float viewZ)
{
    float grid = params.apPlanes.z;
    float2 uv = (float2(id.xy) + 0.5) / grid;
    float2 ndc = uv * 2.0 - 1.0;
    float4 r = mul(params.invProjection, float4(ndc, 1.0, 1.0));
    float3 viewRay = r.xyz / r.w;
    float3 viewPos = viewRay * (viewZ / viewRay.z);
    return mul(params.invView, float4(viewPos, 1.0)).xyz;
}

[shader("compute")]
[numthreads(4, 4, 4)]
void computeMain(uint3 id : SV_DispatchThreadID)
{
    uint gridEdge = uint(params.apPlanes.z + 0.5);
    if (any(id >= gridEdge))
    {
        return;
    }

    float apNear = params.apPlanes.x;
    float apFar = params.apPlanes.y;
    float metresToKm = params.apPlanes.w;

    // Exponential-Z view distance at the slice center (negative-forward), then the world position.
    float wFrac = (float(id.z) + 0.5) / params.apPlanes.z;
    float viewZ = -apNear * pow(apFar / apNear, wFrac);
    float3 worldPos = froxelCenterWorld(id, viewZ);
    float3 eye = mul(params.invView, float4(0.0, 0.0, 0.0, 1.0)).xyz;

    float3 toFroxel = worldPos - eye;
    float distKm = length(toFroxel) * metresToKm;
    float3 viewDir = normalize(toFroxel);
    float3 sunDir = normalize(params.sunDir.xyz);

    // Atmosphere frame: the camera at altitude on the +Y-up planet, agreeing with the sky-view LUT so
    // near fog, mid haze, and the sky behind the ridge read as one atmosphere.
    float r = planetRadius() + max(params.params1.z, 0.5);
    float3 pos = float3(0.0, r, 0.0);

    float cosThetaSun = dot(viewDir, sunDir);
    float phaseR = rayleighPhase(cosThetaSun);
    float phaseM = hgPhase(cosThetaSun, params.params0.w);
    float3 sunColor = params.sunDir.w.xxx;

    // The AP segment is short (<= AP far), so 16 steps resolve it; the loop shape is lifted verbatim
    // from `atmos_skyview.slang` (single-scatter phase + isotropic multiscatter + analytic step integral).
    const int steps = 16;
    float dt = distKm / float(steps);
    float3 transmittance = float3(1.0, 1.0, 1.0);
    float3 radiance = float3(0.0, 0.0, 0.0);

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

        float3 rayleighS = params.rayleigh.xyz * rayD;
        float3 mieS = (params.ozone.w * mieD).xxx;
        float3 ext = rayleighS + mieS + params.ozone.xyz * ozoneD + params.ozone.w * 0.1 * mieD;
        float3 stepT = exp(-ext * dt * 1e-3);

        float sunMu = dot(normalize(sp), sunDir);
        float3 sunT = sampleTransmittance(sr, sunMu);
        float3 ms = sampleMultiScatter(sr, sunMu);

        float3 inScatterSingle = (rayleighS * phaseR + mieS * phaseM) * sunT;
        float3 inScatterMulti = (rayleighS + mieS) * ms;
        float3 inScatter = (inScatterSingle + inScatterMulti) * sunColor;

        float3 sInt = (inScatter - inScatter * stepT) / max(ext, float3(1e-6, 1e-6, 1e-6));
        radiance += transmittance * sInt;
        transmittance *= stepT;
    }

    float meanT = dot(transmittance, float3(1.0, 1.0, 1.0)) / 3.0;
    apVolume[id] = float4(radiance * params.params1.w, meanT);
}
