// Energy-conserving mip-pyramid bloom (Jimenez / Call-of-Duty: Advanced Warfare), run in
// scene-linear radiance on the offscreen color BEFORE the tonemap pass. One entry point, four
// passes selected by `push.pass`:
//   0 — 13-tap Karis-averaged bilinear downsample (36 effective taps via hardware bilinear).
//       The Karis partial average (weight 1/(1+luma)) runs only when `push.karis != 0`, i.e. on
//       the color -> mip0 step, where single-texel HDR fireflies live — the firefly / TAA-stability
//       mechanism. An optional soft-knee prefilter activates only when `push.threshold > 0`.
//   1 — progressive 9-tap 3x3 tent upsample, offsets scaled by `push.filterRadius` UV units, added
//       into the coarser level already written at this mip (read-modify-write). The added
//       contribution is multiplied by `push.mipTint` (identity 1,1,1 when the per-mip stack is off).
//   2 — energy-conserving composite: attenuate the accumulated pyramid by the lens-dirt mask,
//       add the anamorphic streak, then lerp(hdr, bloom * tint, intensity) in place on the color.
//   3 — anamorphic streak: a horizontally-squeezed blur of a bright mip, written to the streak
//       buffer (composited in pass 2). The horizontal reach is `filterRadius * anamorphicRatio`.
// set 0: binding 0 = source (combined image sampler, linear clamp), binding 1 = target
// (rgba16f storage image), binding 2 = lens-dirt mask (asset, or 1x1 white fallback), binding 3 =
// anamorphic streak buffer (transient, or 1x1 white fallback). One PSO covers all four passes.

[[vk::binding(0, 0)]] Sampler2D source;
[[vk::image_format("rgba16f")]]
[[vk::binding(1, 0)]] RWTexture2D<float4> target;
[[vk::binding(2, 0)]] Sampler2D dirtMask;
[[vk::binding(3, 0)]] Sampler2D streak;

// Field grouping keeps every float3 on a 16-byte boundary followed by its trailing scalar, so the
// std140 push-constant layout packs to five vec4 rows (80 bytes) with no padding — matching the
// host `BloomPush`'s `#[repr(C)]`.
struct Push
{
    float3 tint;         // bloom colour, composite pass only
    float filterRadius;  // scatter, UV units (~0.005)
    float intensity;     // energy-conserving lerp weight, composite pass only
    float threshold;     // soft-knee prefilter; 0.0 = off (default)
    uint pass;           // 0 = downsample, 1 = upsample-add, 2 = composite, 3 = streak
    uint karis;          // 1 on the first downsample only
    float3 dirtTint;     // lens-dirt tint, composite pass only
    float dirtIntensity; // lens-dirt mix (0 = no dirt), composite pass only
    float3 mipTint;      // per-mip contribution tint, upsample pass only (identity 1,1,1 when off)
    float anamorphicIntensity; // streak add weight, composite pass only
    float3 anamorphicTint;     // cool streak tint, composite pass only
    float anamorphicRatio;     // horizontal squeeze (~2.0), streak pass only
};
[[vk::push_constant]] Push push;

float luma(float3 c)
{
    // sRGB-ish perceptual weight, matching the Karis average's tonemapped-luma intent.
    return dot(c, float3(0.2126, 0.7152, 0.0722));
}

// Karis average of a 2x2 box: weight each corner by 1/(1+luma) so a lone firefly contributes far
// less than its raw value (the fireflies-do-not-dominate mechanism), returning the weighted mean.
float3 karisAverage(float3 c)
{
    return c / (1.0 + luma(c));
}

// Non-physical soft-knee prefilter (Karis/Jimenez knee). Only active when threshold > 0: below the
// knee the pixel is smoothly rolled to zero, above it passes through. Off (threshold == 0) leaves
// the input untouched so emissive/HDR-bright pixels bloom automatically.
float3 prefilter(float3 c)
{
    if (push.threshold <= 0.0)
    {
        return c;
    }
    const float knee = push.threshold * 0.5;
    float br = max(c.r, max(c.g, c.b));
    float soft = clamp(br - push.threshold + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 1e-4);
    float contribution = max(soft, br - push.threshold) / max(br, 1e-4);
    return c * contribution;
}

// 13-tap downsample: sample the source with the COD kernel (a center 2x2, an inner ring, and an
// outer ring exploiting hardware bilinear), then combine either by the fixed weights (deeper mips)
// or by the firefly-suppressing Karis box average (first mip).
float3 downsample13(float2 uv, float2 srcTexel)
{
    float3 a = source.SampleLevel(uv + srcTexel * float2(-2, 2), 0).rgb;
    float3 b = source.SampleLevel(uv + srcTexel * float2(0, 2), 0).rgb;
    float3 c = source.SampleLevel(uv + srcTexel * float2(2, 2), 0).rgb;

    float3 d = source.SampleLevel(uv + srcTexel * float2(-2, 0), 0).rgb;
    float3 e = source.SampleLevel(uv, 0).rgb;
    float3 f = source.SampleLevel(uv + srcTexel * float2(2, 0), 0).rgb;

    float3 g = source.SampleLevel(uv + srcTexel * float2(-2, -2), 0).rgb;
    float3 h = source.SampleLevel(uv + srcTexel * float2(0, -2), 0).rgb;
    float3 i = source.SampleLevel(uv + srcTexel * float2(2, -2), 0).rgb;

    float3 j = source.SampleLevel(uv + srcTexel * float2(-1, 1), 0).rgb;
    float3 k = source.SampleLevel(uv + srcTexel * float2(1, 1), 0).rgb;
    float3 l = source.SampleLevel(uv + srcTexel * float2(-1, -1), 0).rgb;
    float3 m = source.SampleLevel(uv + srcTexel * float2(1, -1), 0).rgb;

    if (push.karis != 0)
    {
        // Karis average over the five overlapping 2x2 boxes (the firefly/TAA win on mip0 only).
        float3 group0 = (j + k + l + m) * 0.25; // center quad
        float3 group1 = (a + b + d + e) * 0.25; // top-left
        float3 group2 = (b + c + e + f) * 0.25; // top-right
        float3 group3 = (d + e + g + h) * 0.25; // bottom-left
        float3 group4 = (e + f + h + i) * 0.25; // bottom-right
        float w0 = 0.5 / (1.0 + luma(group0));
        float w1 = 0.125 / (1.0 + luma(group1));
        float w2 = 0.125 / (1.0 + luma(group2));
        float w3 = 0.125 / (1.0 + luma(group3));
        float w4 = 0.125 / (1.0 + luma(group4));
        float wsum = w0 + w1 + w2 + w3 + w4;
        float3 sum = group0 * w0 + group1 * w1 + group2 * w2 + group3 * w3 + group4 * w4;
        return prefilter(sum / max(wsum, 1e-4));
    }

    // Fixed-weight combine (deeper mips): the COD 13-tap weights summing to 1.0.
    float3 result = e * 0.125;
    result += (a + c + g + i) * 0.03125;
    result += (b + d + f + h) * 0.0625;
    result += (j + k + l + m) * 0.125;
    return result;
}

// 9-tap 3x3 tent upsample, offsets in UV units scaled by filterRadius (the scatter dial).
float3 upsampleTent(float2 uv, float radius)
{
    float3 sum = source.SampleLevel(uv + float2(-radius, radius), 0).rgb;
    sum += source.SampleLevel(uv + float2(0, radius), 0).rgb * 2.0;
    sum += source.SampleLevel(uv + float2(radius, radius), 0).rgb;
    sum += source.SampleLevel(uv + float2(-radius, 0), 0).rgb * 2.0;
    sum += source.SampleLevel(uv, 0).rgb * 4.0;
    sum += source.SampleLevel(uv + float2(radius, 0), 0).rgb * 2.0;
    sum += source.SampleLevel(uv + float2(-radius, -radius), 0).rgb;
    sum += source.SampleLevel(uv + float2(0, -radius), 0).rgb * 2.0;
    sum += source.SampleLevel(uv + float2(radius, -radius), 0).rgb;
    return sum * (1.0 / 16.0);
}

// Horizontal-only gaussian for the anamorphic streak (Bart Wronski): a wide 13-tap kernel whose
// reach is squeezed horizontally by `anamorphicRatio` (≈2 stretches the glow into a lens streak),
// sampled off the source's own texel size so the kernel widens with the buffer resolution.
float3 streakBlur(float2 uv)
{
    uint srcW;
    uint srcH;
    source.GetDimensions(srcW, srcH);
    float step = (1.0 / float(srcW)) * max(push.anamorphicRatio, 1.0);
    float3 sum = 0.0.xxx;
    float wsum = 0.0;
    [unroll]
    for (int i = -6; i <= 6; ++i)
    {
        float w = exp(-float(i * i) / 18.0);
        sum += source.SampleLevel(uv + float2(float(i) * step, 0.0), 0).rgb * w;
        wsum += w;
    }
    return sum / max(wsum, 1e-4);
}

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
    float2 uv = (float2(px) + 0.5) / float2(width, height);

    if (push.pass == 0)
    {
        uint srcW;
        uint srcH;
        source.GetDimensions(srcW, srcH);
        float2 srcTexel = 1.0 / float2(srcW, srcH);
        float3 down = downsample13(uv, srcTexel);
        target[px] = float4(max(down, 0.0), 1.0);
    }
    else if (push.pass == 1)
    {
        float3 up = upsampleTent(uv, push.filterRadius) * push.mipTint;
        float3 acc = target[px].rgb;
        target[px] = float4(acc + up, 1.0);
    }
    else if (push.pass == 3)
    {
        target[px] = float4(max(streakBlur(uv), 0.0), 1.0);
    }
    else
    {
        float3 hdr = target[px].rgb;
        float3 bloom = source.SampleLevel(uv, 0).rgb;
        // Lens dirt attenuates the existing glow (clamped <= 1, multiplicative — never adds energy).
        float3 dirt = min(dirtMask.SampleLevel(uv, 0).rgb, 1.0.xxx) * push.dirtTint;
        bloom *= lerp(1.0.xxx, dirt, push.dirtIntensity);
        // Anamorphic streak is extra bloom energy, added over the radial result (zero when off).
        bloom += streak.SampleLevel(uv, 0).rgb * push.anamorphicTint * push.anamorphicIntensity;
        bloom *= push.tint;
        target[px] = float4(lerp(hdr, bloom, push.intensity), 1.0);
    }
}
