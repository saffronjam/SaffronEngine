// Exponential moving-average blend from the committed environment cube toward a refreshed target.

[[vk::binding(0, 0)]] SamplerCube<float4> committedCube;
[[vk::binding(1, 0)]] SamplerCube<float4> targetCube;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 0)]] RWTexture2DArray<float4> outCube;

struct BlendPush
{
    float alpha;
};
[[vk::push_constant]] BlendPush pc;

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
    float3 direction = cubeFaceDir(tid.z, uv);
    float4 committed = committedCube.SampleLevel(direction, 0.0);
    float4 target = targetCube.SampleLevel(direction, 0.0);
    outCube[tid] = lerp(committed, target, saturate(pc.alpha));
}
