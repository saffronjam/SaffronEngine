// The mandatory HDR->display tonemap on the offscreen color (an rgba16f storage image). One
// invocation per pixel: read the linear HDR radiance, apply exposure (grade op #1), the scene-linear
// grade (white balance, contrast, saturation, ASC-CDL), the selectable view/display transform +
// gamma 2.2, then the display-space creative 3D LUT look, and write it back. The render graph
// transitions the offscreen Color -> General before this pass and General -> ShaderReadOnly after
// (for the present blit to sample). set 0 binding 0 is the storage image; binding 1 is the per-view
// grade uniform (a dynamic-offset UBO); binding 2 is the creative-look 3D LUT (an identity ramp when
// no look is assigned). The exposure multiplier + operator mode are push constants.
//
// The grade UBO carries a `frozenLook` flag: `0` is the live editor host (ALU grade -> view transform
// -> creative LUT); `1` is the exported player (a single dependent fetch of a baked full-tail LUT over
// the log2 shaper, replacing the ALU tail). One file, two view-modes, one path per binary.

import tonemap_ops;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> target;
[[vk::binding(1, 0)]] ConstantBuffer<GradeUniform> gradeUniform;
[[vk::binding(2, 0)]] Texture3D<float3> creativeLut;

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
    float intensity = gradeUniform.look.x;
    uint lutSize = uint(gradeUniform.look.y);
    uint frozenLook = uint(gradeUniform.look.z);

    float3 lin = color.rgb * push.exposure;       // 1 — exposure (the existing push)
    float3 display;
    if (frozenLook == 1u)
    {
        // Exported player: the ALU grade + view transform + creative LUT are frozen into one baked
        // full-tail LUT. Encode the exposed radiance through the log2 shaper and take one fetch.
        display = sampleCubeTetrahedral(creativeLut, lutShaperEncode(lin), lutSize);
    }
    else
    {
        GradeParams g = unpackGradeParams(gradeUniform);
        float3 graded = grade(lin, g);                    // 2..4 — scene-linear grade
        display = tonemapAndEncode(graded, push.mode);    // view/display transform
        // Display-space creative look, tetrahedral on the bounded [0,1] result, dialled by intensity
        // (0 is the neutral identity ramp — one code path, no presence branch).
        float3 look = sampleCubeTetrahedral(creativeLut, saturate(display), lutSize);
        display = lerp(display, look, intensity);
    }
    target[px] = float4(display, color.a);
}
