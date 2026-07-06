// The HDR->display tonemap operators, shared between the scene tonemap pass (tonemap.slang)
// and the offscreen thumbnail/preview shaders so both encode identically. Resource-free
// module — pure functions only, no bindings / push constants / entry points — so a consumer
// imports the operators without dragging the tonemap pass's storage-image binding along
// (the same split octahedral.slang uses for its encode).

module tonemap_ops;

// Reinhard: simple, flat — caps 1.0->0.5 with no shoulder/path-to-white. Kept for reference/A-B.
public float3 tonemapReinhard(float3 x)
{
    return x / (x + 1.0);
}

// ACES filmic fit (Narkowicz): a proper highlight shoulder + path-to-white, so bright reflections
// of the sun / emissives read as punchy, naturally-desaturating highlights. The engine default.
public float3 tonemapAces(float3 x)
{
    return saturate((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14));
}

// AgX (Benjamin Wrensch / Godot-Blender minimal fit): compresses extreme highlights more gracefully
// than ACES and avoids ACES's hue shift, so saturated metals (gold) keep their hue into the
// highlights. Log2 encode -> per-channel polynomial sigmoid (the inset/outset matrices are folded
// into the curve for the real-time fit).
public float3 agxDefaultContrastApprox(float3 x)
{
    float3 x2 = x * x;
    float3 x4 = x2 * x2;
    return 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 +
           0.1191 * x - 0.00232;
}

public float3 tonemapAgx(float3 color)
{
    const float3x3 agxMat = float3x3(
        0.842479062253094, 0.0423282422610123, 0.0423756549057051,
        0.0784335999999992, 0.878468636469772, 0.0784336,
        0.0792237451477643, 0.0791661274605434, 0.879142973793104);
    const float3x3 agxMatInv = float3x3(
        1.19687900512017, -0.0528968517574562, -0.0529716355144438,
        -0.0980208811401368, 1.15190312990417, -0.0980434501171241,
        -0.0990297440797205, -0.0989611768448433, 1.15107367264116);
    const float minEv = -12.47393;
    const float maxEv = 4.026069;

    color = mul(agxMat, color);
    color = clamp(log2(max(color, 1e-10)), minEv, maxEv);
    color = (color - minEv) / (maxEv - minEv);
    color = agxDefaultContrastApprox(color);
    color = mul(agxMatInv, color);
    return saturate(color);
}

// Khronos PBR Neutral: tuned to preserve material color/saturation (a gold sphere stays gold),
// ideal for asset thumbnails. Desaturates only the brightest highlights.
public float3 tonemapPbrNeutral(float3 color)
{
    const float startCompression = 0.8 - 0.04;
    const float desaturation = 0.15;
    float x = min(color.r, min(color.g, color.b));
    float offset = x < 0.08 ? x - 6.25 * x * x : 0.04;
    color -= offset;
    float peak = max(color.r, max(color.g, color.b));
    if (peak < startCompression)
    {
        return color;
    }
    float d = 1.0 - startCompression;
    float newPeak = 1.0 - d * d / (peak + d - startCompression);
    color *= newPeak / peak;
    float g = 1.0 - 1.0 / (desaturation * (peak - newPeak) + 1.0);
    return lerp(color, float3(newPeak), g);
}

// Apply the selected operator, then the display transfer: AgX already bakes its own transfer, so
// the others get the 2.2 gamma. `mode` matches TonemapMode (0 Reinhard, 1 ACES, 2 AgX, 3 PBR-Neutral).
// The single point every surface (scene + thumbnails) shares so they encode identically.
public float3 tonemapAndEncode(float3 hdr, uint mode)
{
    float3 mapped;
    if (mode == 0u) { mapped = tonemapReinhard(hdr); }
    else if (mode == 2u) { mapped = tonemapAgx(hdr); }
    else if (mode == 3u) { mapped = tonemapPbrNeutral(hdr); }
    else { mapped = tonemapAces(hdr); }
    if (mode != 2u)
    {
        mapped = pow(saturate(mapped), float3(1.0 / 2.2));
    }
    return mapped;
}
