// Atmosphere envCube fill: samples the sky-view LUT by the (azimuth, elevation) of each cube
// direction and adds the sun disk, writing into the 6-layer rgba16f environment cube. Same
// output contract as ibl_skygen (set 0 binding 1 = storage cube), so the IBL convolution chain
// and the visible-sky pass consume it unchanged. Binding 0 = sky-view LUT sampler,
// binding 2 = transmittance LUT sampler.

[[vk::binding(0, 0)]] Sampler2D<float4> skyViewLut;
[[vk::binding(2, 0)]] Sampler2D<float4> transmittanceLut;
[[vk::image_format("rgba16f")]]
[[vk::binding(1, 0)]] RWTexture2DArray<float4> outCube;

struct AtmosParams
{
    float4 sunDir;    // xyz = dir to sun, w = sun intensity
    float4 rayleigh;
    float4 ozone;
    float4 params0;   // planetRadius, atmosphereHeight, mieScaleHeight, mieAnisotropy
    float4 params1;   // sunDiskAngularRadius, sunDiskIntensity, cameraAltitude, moon illuminated fraction
    float4 moon;      // xyz = dir to moon, w = moon intensity trim
    float4 moonDisk;  // angular radius, disk trim, earthshine, per-pixel transmittance
};
[[vk::push_constant]] AtmosParams pc;

static const float PI = 3.14159265;
static const float SOLAR_ILLUMINANCE_TOA = 1.2;
static const float LUNAR_ILLUMINANCE_FULL = 0.0000026;

float3 cubeFaceDir(uint face, float2 uv)
{
    float3 d;
    if (face == 0)      { d = float3( 1.0, -uv.y, -uv.x); }
    else if (face == 1) { d = float3(-1.0, -uv.y,  uv.x); }
    else if (face == 2) { d = float3( uv.x,  1.0,  uv.y); }
    else if (face == 3) { d = float3( uv.x, -1.0, -uv.y); }
    else if (face == 4) { d = float3( uv.x, -uv.y,  1.0); }
    else                { d = float3(-uv.x, -uv.y, -1.0); }
    return normalize(d);
}

// Inverse of the sky-view LUT mapping: world dir -> (u = azimuth, v = horizon-densified elevation).
float2 dirToSkyViewUv(float3 dir)
{
    float azimuth = atan2(dir.z, dir.x);
    if (azimuth < 0.0) { azimuth += 2.0 * PI; }
    float elevation = asin(clamp(dir.y, -1.0, 1.0));
    float u = azimuth / (2.0 * PI);
    // Invert v = 0.5 +/- sqrt(|el| / (pi/2)) * 0.5.
    float a = abs(elevation) / (PI * 0.5);
    float v = 0.5 + sign(elevation) * sqrt(saturate(a)) * 0.5;
    return float2(u, v);
}

float3 atmosphereTransmittance(float3 direction)
{
    float u = direction.y * 0.5 + 0.5;
    float v = saturate(0.5 / pc.params0.y);
    return transmittanceLut.SampleLevel(float2(u, v), 0.0).rgb;
}

float solidAngle(float radius)
{
    return max(2.0 * PI * (1.0 - cos(radius)), 1e-7);
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    uint layers;
    outCube.GetDimensions(width, height, layers);
    if (tid.x >= width || tid.y >= height || tid.z >= layers)
    {
        return;
    }

    float2 uv = (float2(tid.xy) + 0.5) / float2(width, height) * 2.0 - 1.0;
    float3 dir = cubeFaceDir(tid.z, uv);

    float2 svUv = dirToSkyViewUv(dir);
    float3 sky = skyViewLut.SampleLevel(svUv, 0.0).rgb;

    // Solar radiance shares the atmosphere transmittance and illuminance used by the key light.
    float3 sunDir = normalize(pc.sunDir.xyz);
    float cosAngle = dot(dir, sunDir);
    float cosRadius = cos(pc.params1.x);
    float disk = smoothstep(cosRadius, cosRadius + 0.0002, cosAngle);
    float angle = acos(clamp(cosAngle, -1.0, 1.0));
    float radial = saturate(angle / max(pc.params1.x, 1e-5));
    float limbMu = sqrt(max(1.0 - radial * radial, 0.0));
    float limb = 1.0 - 0.6 * (1.0 - limbMu);
    float3 sunTransmittance = atmosphereTransmittance(
        pc.moonDisk.w > 0.5 ? dir : sunDir);
    float3 sunRadiance = SOLAR_ILLUMINANCE_TOA * pc.sunDir.w
        * sunTransmittance / solidAngle(pc.params1.x);
    sky += disk * limb * pc.params1.y * sunRadiance;

    // The lunar sphere uses a Lommel-Seeliger response plus an opposition surge. Its
    // terminator follows the solar direction; earthshine lifts only the dark side.
    float3 moonDir = normalize(pc.moon.xyz);
    float moonCos = dot(dir, moonDir);
    float moonMask = smoothstep(cos(pc.moonDisk.x), cos(pc.moonDisk.x) + 0.0002, moonCos);
    if (moonMask > 0.0 && pc.moon.w > 0.0)
    {
        float3 tangent = normalize(abs(moonDir.y) < 0.99
            ? cross(float3(0.0, 1.0, 0.0), moonDir)
            : cross(float3(1.0, 0.0, 0.0), moonDir));
        float3 bitangent = cross(moonDir, tangent);
        float scale = max(sin(pc.moonDisk.x), 1e-5);
        float2 diskPos = float2(dot(dir, tangent), dot(dir, bitangent)) / scale;
        float facing = sqrt(max(1.0 - dot(diskPos, diskPos), 0.0));
        float3 normal = normalize(-moonDir * facing + tangent * diskPos.x + bitangent * diskPos.y);
        float3 toObserver = -moonDir;
        float mu = max(dot(normal, toObserver), 0.0);
        float mu0 = max(dot(normal, sunDir), 0.0);
        float lommelSeeliger = mu0 / max(mu0 + mu, 1e-4);
        float phaseAngle = acos(clamp(dot(toObserver, sunDir), -1.0, 1.0));
        float opposition = 1.0 + 0.35 / (1.0 + tan(phaseAngle * 0.5) / 0.04);
        float illumination = saturate(pc.params1.w);
        float3 moonTransmittance = atmosphereTransmittance(
            pc.moonDisk.w > 0.5 ? dir : moonDir);
        float3 lit = LUNAR_ILLUMINANCE_FULL * illumination * lommelSeeliger * opposition;
        float3 earthshine = float3(0.45, 0.60, 1.0) * pc.moonDisk.z * (1.0 - illumination);
        float3 moonRadiance = (lit + earthshine * LUNAR_ILLUMINANCE_FULL)
            * pc.moon.w * pc.moonDisk.y * moonTransmittance / solidAngle(pc.moonDisk.x);
        sky += moonMask * moonRadiance;
    }

    outCube[tid] = float4(sky, 1.0);
}
