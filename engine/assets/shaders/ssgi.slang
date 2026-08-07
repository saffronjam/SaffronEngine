// Screen-space global illumination (one bounce). For each pixel, march a few cosine-
// weighted hemisphere rays in view space against the G-buffer depth; on a hit, gather the
// PREVIOUS frame's lit HDR color as incoming indirect radiance. Output a raw indirect-
// radiance map; the ssgi-blur + ssgi-accum passes denoise it before the mesh adds it into
// its ambient term. The per-frame index (params.w) rotates the sample hash so the temporal
// accumulator has decorrelated noise to average, and a luminance clamp at the gather kills
// emissive-bounce fireflies at the source. Set 0: 0 = G-buffer sampler (normal.xyz,
// viewZ.a), 1 = previous-frame color sampler, 2 = SSGI out (rgba16f).

[[vk::binding(0, 0)]] Sampler2D gbuffer;
[[vk::binding(1, 0)]] Sampler2D prevColor;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 0)]] RWTexture2D<float4> outGi;

struct Push
{
    float4x4 projection;     // view -> clip
    float4x4 invProjection;  // clip -> view
    float4 params;           // x = radius (view units), y = intensity, z = step count, w = frame index
    float4 params2;          // x = ray count (cosine-hemisphere samples per pixel)
};
[[vk::push_constant]] Push pc;

static const float PI = 3.14159265;

float3 viewPosFromUv(float2 uv, float viewZ)
{
    float2 ndc = uv * 2.0 - 1.0;
    float4 r = mul(pc.invProjection, float4(ndc, 1.0, 1.0));
    float3 ray = r.xyz / r.w;
    return ray * (viewZ / ray.z);
}

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint width;
    uint height;
    outGi.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 texel = 1.0 / float2(width, height);
    float2 uv = (float2(tid.xy) + 0.5) * texel;
    float4 g = gbuffer.SampleLevel(uv, 0.0);
    float viewZ = g.a;
    if (viewZ > -1e-4)  // background contributes no indirect
    {
        outGi[tid.xy] = float4(0.0, 0.0, 0.0, viewZ);
        return;
    }
    float3 p = viewPosFromUv(uv, viewZ);
    float3 n = normalize(g.xyz);

    // Build a tangent basis to orient the cosine hemisphere around the view-space normal.
    float3 up = abs(n.z) < 0.999 ? float3(0.0, 0.0, 1.0) : float3(1.0, 0.0, 0.0);
    float3 t = normalize(cross(up, n));
    float3 b = cross(n, t);

    float radius = pc.params.x;
    uint steps = uint(pc.params.z);
    // Interleaved gradient noise (low-discrepancy, blue-noise-like in screen space) rotated by the
    // frame index through the golden ratio, so the per-pixel sample decorrelates each frame and the
    // temporal accumulator converges to a clean result. A white-noise hash leaves a low-frequency
    // residual the spatial + temporal denoise cannot remove (visible crawling grain).
    float ign = frac(52.9829189 * frac(0.06711056 * float(tid.x) + 0.00583715 * float(tid.y)));
    float rnd = frac(ign + pc.params.w * 0.6180339887);

    uint rayCount = max(1u, uint(pc.params2.x));
    float3 indirect = float3(0.0);
    for (uint r = 0; r < rayCount; r = r + 1)
    {
        // Cosine-weighted hemisphere sample (concentric-ish via the hash + golden angle).
        float u1 = frac(rnd + float(r) / float(rayCount));
        float u2 = frac(rnd * 1.618 + float(r) * 0.382);
        float r2 = sqrt(u1);
        float phi = 2.0 * PI * u2;
        float3 local = float3(r2 * cos(phi), r2 * sin(phi), sqrt(max(0.0, 1.0 - u1)));
        float3 dir = normalize(local.x * t + local.y * b + local.z * n);

        // March the ray; on the first step where it dips behind stored geometry, gather.
        for (uint s = 1; s <= steps; s = s + 1)
        {
            float3 sp = p + dir * radius * (float(s) / float(steps));
            float4 clip = mul(pc.projection, float4(sp, 1.0));
            if (clip.w <= 0.0) { break; }
            float2 suv = (clip.xy / clip.w) * 0.5 + 0.5;
            if (suv.x < 0.0 || suv.x > 1.0 || suv.y < 0.0 || suv.y > 1.0) { break; }
            float surfZ = gbuffer.SampleLevel(suv, 0.0).a;
            if (surfZ > -1e-4) { continue; }
            float diff = surfZ - sp.z;  // >0 when the stored surface is nearer than the ray
            if (diff > 0.02 && diff < radius * 0.5)
            {
                // Hit: the surface at suv faces back toward us -> its lit color bounces here.
                // Clamp luminance so a single bright emitter caught by one ray cannot spike
                // this pixel far above its neighbours (the firefly the blur would smear).
                float3 c = prevColor.SampleLevel(suv, 0.0).rgb;
                float lum = max(c.r, max(c.g, c.b));
                const float maxLum = 8.0;
                if (lum > maxLum)
                {
                    c *= maxLum / lum;
                }
                indirect += c;
                break;
            }
        }
    }
    indirect = indirect / float(rayCount) * pc.params.y;
    outGi[tid.xy] = float4(indirect, viewZ);
}
