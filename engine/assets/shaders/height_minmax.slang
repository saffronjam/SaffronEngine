// GPU min/max height-pyramid build. Level 0 copies the height channel (R) as `(h, h)`;
// each coarser level takes the exact `(min, max)` of its children, so every texel is a
// conservative bound over the source region it covers. Not a filtered mip chain: the prism
// march and offset-limited parallax need the bound, and linear filtering would blend min
// with max and break it. Levels follow Vulkan mip dims (`max(d >> i, 1)`); an odd source
// edge folds its last row/column into the edge texel so no source texel escapes the bound.
//
// Two modes (push.dst.z): 0 init (heightTex → dst mip 0), 1 reduce (src mip → dst mip).

[[vk::binding(0, 0)]] Texture2D<float4> heightTex;
[[vk::image_format("rg32f")]]
[[vk::binding(1, 0)]] RWTexture2D<float2> srcMip;
[[vk::image_format("rg32f")]]
[[vk::binding(2, 0)]] RWTexture2D<float2> dstMip;

struct Push
{
    uint4 dst; // x = dst width, y = dst height, z = mode (0 init, 1 reduce)
    uint4 src; // xy = src level dims (reduce)
};
[[vk::push_constant]] Push pc;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 id: SV_DispatchThreadID)
{
    if (id.x >= pc.dst.x || id.y >= pc.dst.y)
    {
        return;
    }
    if (pc.dst.z == 0)
    {
        float h = heightTex.Load(int3(id.xy, 0)).r;
        dstMip[id.xy] = float2(h, h);
        return;
    }
    // The edge texel of an odd source dimension absorbs the leftover row/column.
    uint nx = ((pc.src.x & 1u) != 0u && id.x == pc.dst.x - 1u) ? 3u : 2u;
    uint ny = ((pc.src.y & 1u) != 0u && id.y == pc.dst.y - 1u) ? 3u : 2u;
    float mn = 1e30;
    float mx = -1e30;
    for (uint dy = 0; dy < ny; ++dy)
    {
        for (uint dx = 0; dx < nx; ++dx)
        {
            uint2 s = id.xy * 2u + uint2(dx, dy);
            if (s.x < pc.src.x && s.y < pc.src.y)
            {
                float2 t = srcMip[s];
                mn = min(mn, t.x);
                mx = max(mx, t.y);
            }
        }
    }
    dstMip[id.xy] = float2(mn, mx);
}
