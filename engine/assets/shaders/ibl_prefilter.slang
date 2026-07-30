// Prefiltered specular environment: GGX importance-sample the environment for a given
// roughness (one dispatch per scheduled row slice; the push carries roughness and row range).
// Set 0: binding 0 = environment cube (sampler), binding 1 = this mip's cube storage view.

static const float PI = 3.14159265359;

[[vk::binding(0, 0)]] SamplerCube envCube;
[[vk::image_format("rgba16f")]]
[[vk::binding(1, 0)]] RWTexture2DArray<float4> outCube;

struct Push
{
    float roughness;
    float blendAlpha;
    uint rowOffset;
    uint rowCount;
};
[[vk::push_constant]] Push push;

// The source env cube's mip-0 face size, for the filtered-importance mip select below. Must match
// IBL_ENV_SIZE (rendering/src/ibl.rs); update both together if the env resolution changes.
static const float EnvSize = 256.0;

// GGX normal distribution, for the per-sample PDF that drives the source-mip selection below.
float distributionGGX(float ndoth, float a)
{
    float a2 = a * a;
    float d = ndoth * ndoth * (a2 - 1.0) + 1.0;
    return a2 / max(PI * d * d, 1e-5);
}

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

float radicalInverseVdC(uint bits)
{
    bits = (bits << 16u) | (bits >> 16u);
    bits = ((bits & 0x55555555u) << 1u) | ((bits & 0xAAAAAAAAu) >> 1u);
    bits = ((bits & 0x33333333u) << 2u) | ((bits & 0xCCCCCCCCu) >> 2u);
    bits = ((bits & 0x0F0F0F0Fu) << 4u) | ((bits & 0xF0F0F0F0u) >> 4u);
    bits = ((bits & 0x00FF00FFu) << 8u) | ((bits & 0xFF00FF00u) >> 8u);
    return float(bits) * 2.3283064365386963e-10;
}

float2 hammersley(uint i, uint n)
{
    return float2(float(i) / float(n), radicalInverseVdC(i));
}

float3 importanceSampleGGX(float2 xi, float3 n, float roughness)
{
    float a = roughness * roughness;
    float phi = 2.0 * PI * xi.x;
    float cosT = sqrt((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y));
    float sinT = sqrt(1.0 - cosT * cosT);
    float3 h = float3(cos(phi) * sinT, sin(phi) * sinT, cosT);
    float3 up = abs(n.z) < 0.999 ? float3(0.0, 0.0, 1.0) : float3(1.0, 0.0, 0.0);
    float3 tx = normalize(cross(up, n));
    float3 ty = cross(n, tx);
    return normalize(tx * h.x + ty * h.y + n * h.z);
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    uint layers;
    outCube.GetDimensions(width, height, layers);
    uint outputY = tid.y + push.rowOffset;
    if (tid.x >= width || tid.y >= push.rowCount || outputY >= height || tid.z >= layers)
    {
        return;
    }
    float2 uv = (float2(tid.x, outputY) + 0.5) / float2(width, height) * 2.0 - 1.0;
    float3 n = cubeFaceDir(tid.z, uv);
    float3 r = n;
    float3 v = n;  // split-sum approximation: view = normal = reflection

    const uint sampleCount = 128;
    float3 prefiltered = float3(0.0);
    float totalWeight = 0.0;
    for (uint i = 0; i < sampleCount; i = i + 1)
    {
        float2 xi = hammersley(i, sampleCount);
        float3 h = importanceSampleGGX(xi, n, push.roughness);
        float3 l = normalize(2.0 * dot(v, h) * h - v);
        float ndotl = max(dot(n, l), 0.0);
        if (ndotl > 0.0)
        {
            // Filtered importance sampling (Karis 2013 / Frostbite / Filament): sample a SOURCE mip
            // chosen from this sample's solid angle vs a texel's, so each importance sample reads a
            // pre-averaged region of the env instead of one aliased mip-0 texel. This is what kills
            // fireflies + aliasing with a low sample count (a single bright emissive texel gets
            // averaged into a coarse mip before the GGX sum), so the hard luminance clamp can go.
            float a = push.roughness * push.roughness;
            float ndoth = max(dot(n, h), 0.0);
            float vdoth = max(dot(v, h), 1e-4);
            float d = distributionGGX(ndoth, a);
            float pdf = (d * ndoth / (4.0 * vdoth)) + 1e-4;
            float saTexel = 4.0 * PI / (6.0 * EnvSize * EnvSize);
            float saSample = 1.0 / (float(sampleCount) * pdf);
            float mip = push.roughness < 1e-3 ? 0.0 : max(0.5 * log2(saSample / saTexel), 0.0);
            // No firefly clamp: filtered importance sampling over the source mip chain is the
            // variance control. A bright emissive texel is pre-averaged into the coarse mip this
            // sample reads, so it cannot scatter and the reflection keeps its true energy.
            prefiltered += envCube.SampleLevel(l, mip).rgb * ndotl;
            totalWeight += ndotl;
        }
    }
    prefiltered = totalWeight > 0.0 ? prefiltered / totalWeight : envCube.SampleLevel(n, 0.0).rgb;
    uint3 output = uint3(tid.x, outputY, tid.z);
    float3 previous = outCube[output].rgb;
    outCube[output] = float4(lerp(previous, prefiltered, push.blendAlpha), 1.0);
}
