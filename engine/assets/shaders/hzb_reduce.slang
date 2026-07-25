// Hierarchical-Z pyramid reduction: folds mip N-1 into mip N as a conservative MAX,
// covering every source texel (odd source extents fold the trailing row/column into the
// last destination texel). `hzb_copy` documents the depth convention.

[[vk::image_format("r32f")]]
[[vk::binding(0, 0)]] RWTexture2D<float> src;
[[vk::image_format("r32f")]]
[[vk::binding(1, 0)]] RWTexture2D<float> dst;

struct Push
{
    uint2 srcExtent;
    uint2 dstExtent;
};
[[vk::push_constant]] Push pc;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    if (tid.x >= pc.dstExtent.x || tid.y >= pc.dstExtent.y) { return; }
    uint2 first = tid.xy * 2u;
    uint2 last = min(first + 1u, pc.srcExtent - 1u);
    if (tid.x + 1u == pc.dstExtent.x) { last.x = pc.srcExtent.x - 1u; }
    if (tid.y + 1u == pc.dstExtent.y) { last.y = pc.srcExtent.y - 1u; }
    float value = 0.0;
    for (uint y = first.y; y <= last.y; ++y)
    {
        for (uint x = first.x; x <= last.x; ++x)
        {
            value = max(value, src[uint2(x, y)]);
        }
    }
    dst[tid.xy] = value;
}
