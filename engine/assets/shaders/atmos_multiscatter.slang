// Multiple-scattering LUT (Hillaire 2020, isotropic 2nd-order-plus term). For each texel
// (cos sun-zenith, altitude) it integrates single in-scattering over a sphere of directions and
// returns the energy-conserving multi-scatter factor. Set 0 binding 0 = transmittance sampler,
// binding 2 = rgba16f storage output (binding 1 unused here, keeps layoutC uniform).

[[vk::binding(0, 0)]] Sampler2D<float4> transmittanceLut;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 0)]] RWTexture2D<float4> outLut;

struct AtmosParams
{
    float4 sunDir;
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

// Sphere intersection: distance to atmosphere top, and -1 if the ground is hit first.
float rayTopDistance(float r, float mu)
{
    float top = atmosTop();
    float disc = r * r * (mu * mu - 1.0) + top * top;
    return max(0.0, -r * mu + sqrt(max(disc, 0.0)));
}

bool hitsGround(float r, float mu)
{
    float rg = planetRadius();
    float disc = r * r * (mu * mu - 1.0) + rg * rg;
    return mu < 0.0 && disc >= 0.0;
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
    float sunCosZenith = uv.x * 2.0 - 1.0;
    float r = planetRadius() + uv.y * pc.params0.y;
    float3 sunDir = float3(sqrt(max(0.0, 1.0 - sunCosZenith * sunCosZenith)), sunCosZenith, 0.0);
    float3 pos = float3(0.0, r, 0.0);

    // Integrate over a coarse sphere of directions (SQRT_SAMPLES^2 total).
    const int SQRT_SAMPLES = 8;
    float3 lumTotal = float3(0.0, 0.0, 0.0);  // 2nd-order in-scattering toward the eye
    float3 fmsTotal = float3(0.0, 0.0, 0.0);  // fraction of light scattered (for the series sum)
    float invSamples = 1.0 / float(SQRT_SAMPLES * SQRT_SAMPLES);

    for (int si = 0; si < SQRT_SAMPLES; ++si)
    {
        for (int sj = 0; sj < SQRT_SAMPLES; ++sj)
        {
            float theta = PI * (float(si) + 0.5) / float(SQRT_SAMPLES);
            float phi = 2.0 * PI * (float(sj) + 0.5) / float(SQRT_SAMPLES);
            float3 dir = float3(sin(theta) * cos(phi), cos(theta), sin(theta) * sin(phi));

            float mu = dir.y;  // view-zenith cosine of this sample ray at pos (up = +Y)
            float dist = rayTopDistance(r, mu);
            if (hitsGround(r, mu))
            {
                // March only to the ground for rays that hit it.
                float rg = planetRadius();
                float disc = r * r * (mu * mu - 1.0) + rg * rg;
                dist = max(0.0, -r * mu - sqrt(max(disc, 0.0)));
            }

            const int steps = 20;
            float dt = dist / float(steps);
            float3 transmittance = float3(1.0, 1.0, 1.0);
            float3 lum = float3(0.0, 0.0, 0.0);
            float3 fms = float3(0.0, 0.0, 0.0);

            for (int k = 0; k < steps; ++k)
            {
                float t = (float(k) + 0.5) * dt;
                float3 sp = pos + dir * t;
                float sr = max(length(sp), planetRadius());
                float h = sr - planetRadius();
                float rayD;
                float mieD;
                float ozoneD;
                densities(h, rayD, mieD, ozoneD);

                float3 scatter = pc.rayleigh.xyz * rayD + pc.ozone.w * mieD;
                float3 ext = scatter + pc.ozone.xyz * ozoneD + pc.ozone.w * 0.1 * mieD;
                float3 stepT = exp(-ext * dt * 1e-3);

                // Light reaching this sample directly from the sun.
                float sunMu = dot(normalize(sp), sunDir);
                float3 sunT = sampleTransmittance(sr, sunMu);
                // Uniform (isotropic) phase for the multi-scatter pass.
                float3 inScatter = scatter * sunT * (1.0 / (4.0 * PI));

                // Divide by the raw ext (not ext*1e-3): inScatter/ext is a unit-independent
                // albedo; the km<->Mm conversion already lives in stepT's optical depth. An
                // extra 1e-3 here over-brightens 1000x and makes fms diverge.
                float3 sInt = (inScatter - inScatter * stepT) / max(ext, float3(1e-6, 1e-6, 1e-6));
                lum += transmittance * sInt;
                // fms accumulates the scattered fraction (uniform-lit) for the geometric series.
                float3 fInt = (scatter * (1.0 / (4.0 * PI)) - scatter * (1.0 / (4.0 * PI)) * stepT) /
                              max(ext, float3(1e-6, 1e-6, 1e-6));
                fms += transmittance * fInt;
                transmittance *= stepT;
            }

            lumTotal += lum * invSamples * (4.0 * PI);
            fmsTotal += fms * invSamples * (4.0 * PI);
        }
    }

    // Energy-conserving infinite-scattering series: L_2nd * 1/(1 - f_ms).
    float3 psi = lumTotal / max(float3(1.0, 1.0, 1.0) - fmsTotal, float3(1e-3, 1e-3, 1e-3));
    outLut[tid.xy] = float4(psi, 1.0);
}
