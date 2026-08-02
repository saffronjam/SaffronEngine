// Horizon-based ambient occlusion (GTAO-lite) on the thin G-buffer. For each pixel it
// reconstructs the view-space position from the stored view-Z, then for a few azimuth
// slices marches a few steps each side searching for the largest horizon angle, and
// integrates the visible cosine cone. Output is `r` = AO factor in [0,1] (1 = open), `g` = the
// view-Z it was traced at, which the bilateral denoise weights its taps by.
// Set 0: binding 0 = G-buffer (viewNormal.xyz, viewZ in .a) sampler; binding 1 = AO out.

[[vk::binding(0, 0)]] Sampler2D gbuffer;
[[vk::image_format("rgba16f")]]
[[vk::binding(1, 0)]] RWTexture2D<float4> aoOut;

struct Push
{
    float4x4 invProjection;  // clip -> view
    float4 params;           // x = radius (view units), y = strength, zw = unused
};
[[vk::push_constant]] Push pc;

static const float PI = 3.14159265;

// View-space position of a pixel, from its stored view-Z + a clip-space ray.
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
    // Bounds + uv come from the (possibly half-res) AO output, not the full-res G-buffer, so a
    // half-res trace maps each output texel to the right full-res G-buffer sample.
    uint width;
    uint height;
    aoOut.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height)
    {
        return;
    }
    float2 texel = 1.0 / float2(width, height);
    float2 uv = (float2(tid.xy) + 0.5) * texel;

    float4 g = gbuffer.SampleLevel(uv, 0.0);
    float viewZ = g.a;
    // Background (no geometry: viewZ ~ 0 from the clear) is fully open.
    if (viewZ > -1e-4)
    {
        aoOut[tid.xy] = float4(1.0, viewZ, 0.0, 0.0);
        return;
    }
    float3 n = normalize(g.xyz);
    float3 p = viewPosFromUv(uv, viewZ);

    const float radius = pc.params.x;            // occlusion radius in view-space units
    const uint sliceCount = 4;
    const uint stepCount = 6;
    // The world radius projects to a screen-space radius that shrinks with distance; clamp
    // so near surfaces don't over-sample and far ones still register.
    float screenRadius = clamp(radius / max(-viewZ, 0.1), 0.02, 0.25);
    // A per-pixel rotation breaks up banding (hash of the pixel coords).
    float rnd = frac(sin(dot(float2(tid.xy), float2(12.9898, 78.233))) * 43758.5453);

    // HBAO-style hemisphere occlusion: each tap that rises above the tangent plane and lies
    // within the radius occludes, weighted by how far above the surface it is + a range
    // falloff. Averaged over many directions this darkens creases + contact points.
    float occlusion = 0.0;
    float total = 0.0;
    const float bias = 0.02;
    for (uint s = 0; s < sliceCount; s = s + 1)
    {
        float phi = (float(s) + rnd) / float(sliceCount) * 2.0 * PI;
        float2 dir = float2(cos(phi), sin(phi));
        for (uint t = 1; t <= stepCount; t = t + 1)
        {
            float2 sampleUv = uv + dir * screenRadius * (float(t) / float(stepCount));
            total += 1.0;
            if (sampleUv.x < 0.0 || sampleUv.x > 1.0 || sampleUv.y < 0.0 || sampleUv.y > 1.0)
            {
                continue;
            }
            float4 sg = gbuffer.SampleLevel(sampleUv, 0.0);
            if (sg.a > -1e-4)
            {
                continue;  // background: no occluder
            }
            float3 sp = viewPosFromUv(sampleUv, sg.a);
            float3 d = sp - p;
            float len = length(d);
            if (len < 1e-4)
            {
                continue;
            }
            float ndotd = dot(n, d / len);
            float rangeCheck = saturate(1.0 - (len - radius) / radius);  // fade past the radius
            occlusion += max(ndotd - bias, 0.0) * rangeCheck;
        }
    }
    occlusion = total > 0.0 ? occlusion / total : 0.0;
    float ao = saturate(1.0 - occlusion * pc.params.y);
    aoOut[tid.xy] = float4(ao, viewZ, 0.0, 0.0);
}
