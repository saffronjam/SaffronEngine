// The look bake: folds the scene-linear grade + view/display transform + creative 3D LUT into one
// `33³` display-referred table over a log2 shaper, so the exported player runs a single dependent LUT
// fetch instead of the whole ALU tail. One invocation per cell of a `33×33×33` storage 3D image; each
// cell decodes its shaper coordinate to scene-linear radiance, runs the *same* shared helpers as the
// live tonemap pass (grade -> tonemapAndEncode -> creative LUT), and writes the display result — so a
// baked look is bit-identical to the live ALU up to shaper node quantization.
//
// set 0 binding 0 is the baked output (rgba16f storage 3D image); binding 1 the frozen grade uniform;
// binding 2 the bound creative LUT (an identity ramp when no look is assigned).

import tonemap_ops;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture3D<float4> bakedLut;
[[vk::binding(1, 0)]] ConstantBuffer<GradeUniform> gradeUniform;
[[vk::binding(2, 0)]] Texture3D<float3> creativeLut;

struct Push
{
    uint size; // the baked grid resolution per axis (33)
    uint mode; // the view/display transform (TonemapMode)
};
[[vk::push_constant]] Push push;

[shader("compute")]
[numthreads(4, 4, 4)]
void computeMain(uint3 gid : SV_DispatchThreadID)
{
    if (gid.x >= push.size || gid.y >= push.size || gid.z >= push.size)
    {
        return;
    }

    float3 t = float3(gid) / float(push.size - 1);
    float3 lin = lutShaperDecode(t);                       // log2 shaper: t -> scene-linear
    GradeParams g = unpackGradeParams(gradeUniform);
    float3 graded = grade(lin, g);                         // the frozen scene-linear grade
    float3 display = tonemapAndEncode(graded, push.mode);  // the view/display transform
    float3 look = sampleCubeTetrahedral(creativeLut, saturate(display), uint(gradeUniform.look.y));
    display = lerp(display, look, gradeUniform.look.x);
    bakedLut[gid] = float4(display, 1.0);
}
