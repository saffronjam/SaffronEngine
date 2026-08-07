// Hierarchical-Z pyramid seed. Depth convention: LESS with far = 1.0, so the pyramid
// stores the MAXIMUM (farthest) depth of the texels each level covers — a box whose
// nearest depth exceeds the covering tile's value is occluded. Seeds mip 0 from the 1x
// scene depth; `hzb_reduce` folds the remaining levels.

[[vk::binding(0, 0)]] Sampler2D<float> sceneDepth;
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
    dst[tid.xy] = sceneDepth.Load(int3(tid.xy, 0));
}
