import sky_sh;

[[vk::binding(0, 0)]] SamplerCube envCube;
[[vk::binding(1, 0)]] RWStructuredBuffer<float4> shCoefficients;

static const uint ThreadCount = 64;
static const uint FaceSize = 32;
static const float ProjectionMip = 3.0;
groupshared float3 partial[9 * ThreadCount];

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

[shader("compute")]
[numthreads(ThreadCount, 1, 1)]
void computeMain(uint3 groupThread : SV_GroupThreadID)
{
    uint lane = groupThread.x;
    float3 sums[9];
    for (uint c = 0; c < 9; c = c + 1)
    {
        sums[c] = float3(0.0);
    }

    uint texelCount = FaceSize * FaceSize * 6;
    for (uint index = lane; index < texelCount; index = index + ThreadCount)
    {
        uint faceTexel = index % (FaceSize * FaceSize);
        uint face = index / (FaceSize * FaceSize);
        uint x = faceTexel % FaceSize;
        uint y = faceTexel / FaceSize;
        float2 uv = (float2(x, y) + 0.5) / float(FaceSize) * 2.0 - 1.0;
        float3 direction = cubeFaceDir(face, uv);
        float weight = 4.0 / pow(1.0 + dot(uv, uv), 1.5) / float(FaceSize * FaceSize);
        float3 radiance = envCube.SampleLevel(direction, ProjectionMip).rgb;
        float basis[9];
        skyShBasis(direction, basis);
        for (uint c = 0; c < 9; c = c + 1)
        {
            sums[c] += radiance * (basis[c] * weight);
        }
    }

    for (uint c = 0; c < 9; c = c + 1)
    {
        partial[c * ThreadCount + lane] = sums[c];
    }
    GroupMemoryBarrierWithGroupSync();

    for (uint stride = ThreadCount / 2; stride > 0; stride = stride / 2)
    {
        if (lane < stride)
        {
            for (uint c = 0; c < 9; c = c + 1)
            {
                partial[c * ThreadCount + lane] += partial[c * ThreadCount + lane + stride];
            }
        }
        GroupMemoryBarrierWithGroupSync();
    }

    if (lane == 0)
    {
        for (uint c = 0; c < 9; c = c + 1)
        {
            shCoefficients[c] = float4(partial[c * ThreadCount], 0.0);
        }
    }
}
