// The mandatory HDR->display tonemap on the offscreen color (an rgba16f storage image). One
// invocation per pixel: read the linear HDR radiance, apply exposure, a selectable tonemap operator
// + gamma 2.2, write it back. The render graph transitions the offscreen Color -> General before
// this pass and General -> ShaderReadOnly after (for the present blit to sample). set 0 binding 0;
// the exposure multiplier + operator mode are push constants.

import tonemap_ops;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> target;

struct Push
{
    float exposure;  // linear multiplier (exp2 of the EV stop)
    uint mode;       // 0 Reinhard, 1 ACES, 2 AgX, 3 PBR-Neutral (TonemapMode)
};
[[vk::push_constant]] Push push;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    target.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }

    uint2 px = tid.xy;
    float4 color = target[px];
    float3 mapped = tonemapAndEncode(color.rgb * push.exposure, push.mode);
    target[px] = float4(mapped, color.a);
}
