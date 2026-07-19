// Transmittance LUT (Hillaire 2020): for each texel maps (u,v) -> (cos view-zenith, altitude)
// and ray-marches to the atmosphere top, accumulating Rayleigh + Mie + ozone extinction, then
// writes exp(-opticalDepth). No input texture; set 0 binding 0 is the rgba16f storage output.

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> outLut;

// AtmosphereParams + celestial lights, packed into 7 float4s.
struct AtmosParams
{
    float4 sunDir;    // xyz = dir to sun, w = sun intensity
    float4 rayleigh;  // xyz = rayleigh scattering (1/Mm), w = rayleigh scale height (km)
    float4 ozone;     // xyz = ozone absorption (1/Mm), w = mie scattering (1/Mm)
    float4 params0;   // planetRadius, atmosphereHeight, mieScaleHeight, mieAnisotropy
    float4 params1;   // sunDiskAngularRadius, sunDiskIntensity, cameraAltitude, unused
    float4 moon;      // xyz = dir to moon, w = moon intensity trim
    float4 moonDisk;  // angular radius, disk trim, earthshine, per-pixel transmittance
};
[[vk::push_constant]] AtmosParams pc;

static const float PI = 3.14159265;

float planetRadius() { return pc.params0.x; }
float atmosTop() { return pc.params0.x + pc.params0.y; }

// Density (relative to sea level) of Rayleigh + Mie + ozone at altitude h (km above the surface).
// Ozone uses a tent peaked at ~25 km (the Earth ozone layer), as in Hillaire/Bruneton.
void densities(float h, out float rayleigh, out float mie, out float ozone)
{
    rayleigh = exp(-max(h, 0.0) / pc.rayleigh.w);
    mie = exp(-max(h, 0.0) / pc.params0.z);
    ozone = max(0.0, 1.0 - abs(h - 25.0) / 15.0);
}

// Distance from a point at radius r (with view-zenith cosine mu) to the atmosphere top.
float rayTopDistance(float r, float mu)
{
    float top = atmosTop();
    float disc = r * r * (mu * mu - 1.0) + top * top;
    return max(0.0, -r * mu + sqrt(max(disc, 0.0)));
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
    // u -> view-zenith cosine in [-1,1]; v -> altitude in [0, atmosphereHeight].
    float mu = uv.x * 2.0 - 1.0;
    float r = planetRadius() + uv.y * pc.params0.y;

    float dist = rayTopDistance(r, mu);
    const int steps = 40;
    float dt = dist / float(steps);
    float3 opticalDepth = float3(0.0, 0.0, 0.0);
    for (int i = 0; i < steps; ++i)
    {
        float t = (float(i) + 0.5) * dt;
        // Altitude of the sample along the ray from (r, mu).
        float sr = sqrt(max(r * r + t * t + 2.0 * r * t * mu, 0.0));
        float h = sr - planetRadius();
        float rayD;
        float mieD;
        float ozoneD;
        densities(h, rayD, mieD, ozoneD);
        // Extinction = scattering + absorption. Mie absorbs ~10% of its scattering.
        float3 ext = pc.rayleigh.xyz * rayD + (pc.ozone.w * (1.0 + 0.1)) * mieD + pc.ozone.xyz * ozoneD;
        opticalDepth += ext * dt;
    }
    // Coefficients are 1/Mm but lengths are km, so scale by 1e-3.
    float3 transmittance = exp(-opticalDepth * 1e-3);
    outLut[tid.xy] = float4(transmittance, 1.0);
}
