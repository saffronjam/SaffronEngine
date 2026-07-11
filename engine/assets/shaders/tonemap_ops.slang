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

// One masked correction range (Shadows / Midtones / Highlights): an ASC-CDL SOP triplet plus a
// saturation and a contrast, blended into the frame by a smooth luma weight. Neutral is slope 1,
// offset 0, power 1, saturation 1, contrast 1 — an identity that leaves the range untouched.
public struct GradeBlockParams
{
    public float3 slope;      // ASC-CDL S
    public float3 offset;     // ASC-CDL O
    public float3 power;      // ASC-CDL P
    public float saturation;  // around Rec.709 luma
    public float contrast;    // gain around GRADE_PIVOT in log2
};

// The scene-linear grade parameters, unpacked from the per-view grade uniform by the tonemap pass.
// A pure value struct — `grade` is resource-free, so thumbnail/preview shaders that import this
// module without binding the grade uniform are unaffected (they never call it).
public struct GradeParams
{
    public float3x3 whiteBalance; // Bradford chromatic-adaptation matrix (built CPU-side from Temp/Tint)
    public float3 slope;          // ASC-CDL S
    public float3 offset;         // ASC-CDL O
    public float3 power;          // ASC-CDL P
    public float contrast;        // gain around the log2 pivot
    public float pivot;           // 0.18 middle grey
    public float saturation;      // around Rec.709 luma
    public GradeBlockParams shadows;    // masked correction: darks
    public GradeBlockParams midtones;   // masked correction: mids
    public GradeBlockParams highlights; // masked correction: brights
    public float shadowsMax;      // luma where the shadow mask reaches zero (~0.09)
    public float highlightsMin;   // luma where the highlight mask starts to rise (~0.5)
    public float3x3 channelMixer; // row-major 3x3, identity by default
    public float3 splitShadow;    // split-tone shadow tint (0.5 neutral)
    public float3 splitHighlight; // split-tone highlight tint (0.5 neutral)
    public float splitBalance;    // split-tone luma pivot bias (0 neutral)
};

static const float3 kRec709Luma = float3(0.2126, 0.7152, 0.0722);
static const float GRADE_PIVOT = 0.18; // the per-range contrast pivot (middle grey)

float lumaRec709(float3 c)
{
    return dot(c, kRec709Luma);
}

float3 applyContrast(float3 c, float contrast, float pivot)
{
    float3 lg = log2(max(c, 1e-5));
    lg = (lg - log2(pivot)) * contrast + log2(pivot);
    return exp2(lg);
}

float3 applySaturation(float3 c, float saturation)
{
    float luma = lumaRec709(c);
    return lerp(float3(luma, luma, luma), c, saturation);
}

float3 applyCdl(float3 c, float3 slope, float3 offset, float3 power)
{
    return pow(max(slope * c + offset, 0.0), power);
}

// UE Shadows/Midtones/Highlights mask: shadows fall to zero by `shadowsMax`, highlights rise from
// `highlightsMin`, midtones are the smooth remainder. A `smoothstep` overlap (never a hard branch)
// so a pixel near a boundary receives a blend of the two neighbouring corrections and gradients
// never band.
float3 rangeWeights(float l, float shadowsMax, float highlightsMin)
{
    float ws = 1.0 - smoothstep(0.0, shadowsMax, l);
    float wh = smoothstep(highlightsMin, 1.0, l);
    return float3(ws, saturate(1.0 - ws - wh), wh); // shadows, midtones, highlights
}

// One range's correction, reusing the global leaves in the same ASC-CDL order.
float3 gradeTrio(float3 c, GradeBlockParams b)
{
    c = applyCdl(c, b.slope, b.offset, b.power);
    c = applySaturation(c, b.saturation);
    c = applyContrast(c, b.contrast, GRADE_PIVOT);
    return c;
}

// A shadow tint + a highlight tint blended by luma, biased by `balance`. The 0.5-neutral tints are
// doubled so an untinted split (0.5, 0.5) multiplies by 1 (identity).
float3 splitTone(float3 c, float3 shadowTint, float3 highlightTint, float balance)
{
    float t = saturate(lumaRec709(c) + balance);
    return c * (lerp(shadowTint, highlightTint, t) * 2.0);
}

// The scene-linear grade, applied AFTER exposure and BEFORE the view/display transform: the global
// ops in ASC-CDL order (white balance -> contrast around the 0.18 pivot -> saturation -> global
// ASC-CDL SOP), then three luma-masked Shadows/Midtones/Highlights corrections, a 3x3 channel mixer,
// and split-toning. The neutral grade (identity white balance, contrast/saturation 1, slope/power 1,
// offset 0, identity ranges + mixer, 0.5 split tints) is a mathematical identity, so an ungraded
// frame is unchanged.
public float3 grade(float3 c, GradeParams g)
{
    c = mul(g.whiteBalance, c);
    c = applyContrast(c, g.contrast, g.pivot);
    c = applySaturation(c, g.saturation);
    c = applyCdl(c, g.slope, g.offset, g.power);
    float3 w = rangeWeights(lumaRec709(c), g.shadowsMax, g.highlightsMin);
    c = lerp(c, gradeTrio(c, g.shadows), w.x);
    c = lerp(c, gradeTrio(c, g.midtones), w.y);
    c = lerp(c, gradeTrio(c, g.highlights), w.z);
    c = mul(g.channelMixer, c); // 3x3 row-major
    c = splitTone(c, g.splitShadow, g.splitHighlight, g.splitBalance);
    return c;
}

// The log2 shaper the creative-LUT bake maps its `33³` grid against: cube coordinate `t ∈ [0,1]`
// spans scene-linear radiance across EV `[-14, +11]` anchored at 18% grey, so a bounded table can
// hold the whole unbounded HDR→display tail. Shared by the bake (lut_bake.slang), the player's
// frozen-look branch (tonemap.slang), and the `.slut` header the CPU writes.
public static const float LUT_SHAPER_EV_MIN = -14.0;
public static const float LUT_SHAPER_EV_MAX = 11.0;
public static const float LUT_SHAPER_GREY = 0.18;

// Scene-linear radiance -> the `[0,1]` shaper coordinate (per channel), the encode the player runs to
// index the baked full-tail LUT.
public float3 lutShaperEncode(float3 lin)
{
    float3 ev = log2(max(lin, 1e-8) / LUT_SHAPER_GREY);
    return saturate((ev - LUT_SHAPER_EV_MIN) / (LUT_SHAPER_EV_MAX - LUT_SHAPER_EV_MIN));
}

// The `[0,1]` shaper coordinate -> scene-linear radiance, the decode the bake runs per grid cell.
public float3 lutShaperDecode(float3 t)
{
    return LUT_SHAPER_GREY * exp2(lerp(LUT_SHAPER_EV_MIN, LUT_SHAPER_EV_MAX, t));
}

// One clamped integer corner fetch of a 3D LUT (nominal sampler — the tetrahedral blend reads corners
// with `Load`, so clamp-to-edge at the cube faces is all the addressing that matters).
float3 lutLoad(Texture3D<float3> lut, int3 idx, uint n)
{
    int hi = int(n) - 1;
    idx = clamp(idx, int3(0, 0, 0), int3(hi, hi, hi));
    return lut.Load(int4(idx, 0));
}

// Tetrahedral 3D-LUT interpolation on `c ∈ [0,1]` for an `n×n×n` table (the Resolve / hardware-LUT
// blend, not trilinear). The unit cube splits into six tetrahedra by the ordering of the fractional
// coordinates; the sample is the barycentric blend of the four corners of the tetrahedron it lands in,
// which avoids the hue skew trilinear box interpolation shows along the cube diagonal.
public float3 sampleCubeTetrahedral(Texture3D<float3> lut, float3 c, uint n)
{
    float3 p = saturate(c) * float(n - 1);
    int hi = int(n) - 2;
    int3 b = clamp(int3(floor(p)), int3(0, 0, 0), int3(hi, hi, hi));
    float3 f = p - float3(b);
    float3 v000 = lutLoad(lut, b, n);
    float3 v111 = lutLoad(lut, b + int3(1, 1, 1), n);
    if (f.x >= f.y)
    {
        if (f.y >= f.z)
        {
            float3 v100 = lutLoad(lut, b + int3(1, 0, 0), n);
            float3 v110 = lutLoad(lut, b + int3(1, 1, 0), n);
            return (1.0 - f.x) * v000 + (f.x - f.y) * v100 + (f.y - f.z) * v110 + f.z * v111;
        }
        else if (f.x >= f.z)
        {
            float3 v100 = lutLoad(lut, b + int3(1, 0, 0), n);
            float3 v101 = lutLoad(lut, b + int3(1, 0, 1), n);
            return (1.0 - f.x) * v000 + (f.x - f.z) * v100 + (f.z - f.y) * v101 + f.y * v111;
        }
        else
        {
            float3 v001 = lutLoad(lut, b + int3(0, 0, 1), n);
            float3 v101 = lutLoad(lut, b + int3(1, 0, 1), n);
            return (1.0 - f.z) * v000 + (f.z - f.x) * v001 + (f.x - f.y) * v101 + f.y * v111;
        }
    }
    else
    {
        if (f.z >= f.y)
        {
            float3 v001 = lutLoad(lut, b + int3(0, 0, 1), n);
            float3 v011 = lutLoad(lut, b + int3(0, 1, 1), n);
            return (1.0 - f.z) * v000 + (f.z - f.y) * v001 + (f.y - f.x) * v011 + f.x * v111;
        }
        else if (f.z >= f.x)
        {
            float3 v010 = lutLoad(lut, b + int3(0, 1, 0), n);
            float3 v011 = lutLoad(lut, b + int3(0, 1, 1), n);
            return (1.0 - f.y) * v000 + (f.y - f.z) * v010 + (f.z - f.x) * v011 + f.x * v111;
        }
        else
        {
            float3 v010 = lutLoad(lut, b + int3(0, 1, 0), n);
            float3 v110 = lutLoad(lut, b + int3(1, 1, 0), n);
            return (1.0 - f.y) * v000 + (f.y - f.x) * v010 + (f.x - f.z) * v110 + f.z * v111;
        }
    }
}

// The std140 image of the per-view grade uniform (matches `GradeUniform` on the CPU): the Bradford
// white balance as three vec4 rows, the global slope/offset/power, the contrast/pivot/saturation
// scalars, the three masked ranges (SOP with saturation in slope.w + contrast in offset.w), the 3x3
// channel mixer, the two split-tone tints, the range knobs, and the creative-look block
// (`look` = intensity, size, frozen-look flag). Bound at binding 1 of the tonemap set; shared by
// tonemap.slang and lut_bake.slang so both unpack it identically.
public struct GradeUniform
{
    public float4 whiteBalance0;
    public float4 whiteBalance1;
    public float4 whiteBalance2;
    public float4 slope;
    public float4 offset;
    public float4 power;
    public float4 tonal; // contrast, pivot, saturation, pad
    public float4 shadowsSlope;      // xyz slope, w saturation
    public float4 shadowsOffset;     // xyz offset, w contrast
    public float4 shadowsPower;      // xyz power, w pad
    public float4 midtonesSlope;
    public float4 midtonesOffset;
    public float4 midtonesPower;
    public float4 highlightsSlope;
    public float4 highlightsOffset;
    public float4 highlightsPower;
    public float4 mixer0; // channel-mixer row 0
    public float4 mixer1; // channel-mixer row 1
    public float4 mixer2; // channel-mixer row 2
    public float4 splitShadow;    // xyz tint
    public float4 splitHighlight; // xyz tint
    public float4 rangeKnobs;     // x shadowsMax, y highlightsMin, z splitBalance, w pad
    public float4 look;           // x creativeLutIntensity, y creativeLutSize, z frozenLook, w pad
};

// Unpacks the std140 grade uniform into the resource-free `GradeParams` the pure `grade` takes.
public GradeParams unpackGradeParams(GradeUniform u)
{
    GradeParams g;
    g.whiteBalance = float3x3(u.whiteBalance0.xyz, u.whiteBalance1.xyz, u.whiteBalance2.xyz);
    g.slope = u.slope.xyz;
    g.offset = u.offset.xyz;
    g.power = u.power.xyz;
    g.contrast = u.tonal.x;
    g.pivot = u.tonal.y;
    g.saturation = u.tonal.z;

    GradeBlockParams shadows;
    shadows.slope = u.shadowsSlope.xyz;
    shadows.saturation = u.shadowsSlope.w;
    shadows.offset = u.shadowsOffset.xyz;
    shadows.contrast = u.shadowsOffset.w;
    shadows.power = u.shadowsPower.xyz;
    g.shadows = shadows;

    GradeBlockParams midtones;
    midtones.slope = u.midtonesSlope.xyz;
    midtones.saturation = u.midtonesSlope.w;
    midtones.offset = u.midtonesOffset.xyz;
    midtones.contrast = u.midtonesOffset.w;
    midtones.power = u.midtonesPower.xyz;
    g.midtones = midtones;

    GradeBlockParams highlights;
    highlights.slope = u.highlightsSlope.xyz;
    highlights.saturation = u.highlightsSlope.w;
    highlights.offset = u.highlightsOffset.xyz;
    highlights.contrast = u.highlightsOffset.w;
    highlights.power = u.highlightsPower.xyz;
    g.highlights = highlights;

    g.shadowsMax = u.rangeKnobs.x;
    g.highlightsMin = u.rangeKnobs.y;
    g.channelMixer = float3x3(u.mixer0.xyz, u.mixer1.xyz, u.mixer2.xyz);
    g.splitShadow = u.splitShadow.xyz;
    g.splitHighlight = u.splitHighlight.xyz;
    g.splitBalance = u.rangeKnobs.z;
    return g;
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
