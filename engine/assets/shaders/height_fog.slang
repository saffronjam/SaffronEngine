// Analytic exponential height & distance fog composited into the scene-linear HDR offscreen,
// after the scene resolve and before the bloom pyramid. One invocation per pixel: reconstruct the
// receiver's world position from the scene depth, integrate the exponential-in-height density along
// the view ray in closed form (UE `HeightFogCommon.ush` / iquilezles "Better Fog"), clamp the
// opacity, and blend `scene*T + inscatter*(1-T)` in place. In-scatter is the flat `albedo` tinted by
// the Hillaire sky-view LUT (so it reddens toward the sun when the atmosphere is live) plus a
// directional sun-through-haze lobe. The broad layer and an optional ground layer sum into one
// optical depth — never a second effect.
//
// set 0: binding 0 = the offscreen color (rgba16f storage image, GENERAL), binding 1 = the fog
// params (a dynamic-offset UBO), binding 2 = the scene depth (combined image sampler, D32, .r),
// binding 3 = the sky-view LUT (combined image sampler, rgba16f), binding 4 = the integrated froxel
// volume, binding 5 = the aerial-perspective volume, bindings 6/7 = atmosphere LUTs, and bindings
// 8/9 = full-resolution cloud scatter/transmittance and mean front depth. The graph transitions
// Color->General and the depth/LUT to ShaderReadOnly before the dispatch.
//
// The shared transmittance ledger folds fog and aerial perspective as two INDEPENDENT multiplied media
// so near fog, mid haze, and far planetary scattering read as one atmosphere with no double darkening:
// `T = T_fog * T_aerial`, `L = scene*T + inScatter_fog + T_fog*inScatter_aerial` (aerial sits behind the
// near fog along the ray, so its in-scatter is pre-attenuated by `T_fog`). Fog is neutral (`T_fog = 1`,
// `inScatter_fog = 0`) when fog is disabled but AP keeps the composite alive.

import atmos_ap;

static const float PI = 3.14159265358979;
static const float LN2 = 0.6931472;

struct FogLayer
{
    float density;       // sigma at `height`; 0 disables this layer
    float heightFalloff; // exp2 falloff with world-up distance
    float height;        // world-up reference height
    float _pad;
};

struct FogParams
{
    float4x4 invViewProj; // clip -> world for depth reconstruction (unjittered)
    float3 cameraPos;
    float maxOpacity;
    float3 albedo;
    float startDistance;
    float3 emissive;
    float dirExponent;
    float3 sunDir; // world-space direction TOWARD the sun (normalized)
    float useSkyLut; // 1 when the atmosphere sky-view LUT is live, else 0
    float3 dirColor;
    float _pad0;
    FogLayer layer0; // broad layer
    FogLayer layer1; // ground haze
    float4 froxel;     // x = mode (0 analytic, 1 volumetric), y = froxel near, z = froxel far, w = debug
    float3 camForward; // camera view direction (world), for the froxel/AP W depth mapping
    float fogEnabled;  // 1 = fog contributes; 0 = composite runs only for aerial perspective
    float4 aerial;     // x = AP live (atmosphere baked + authored), y = AP near, z = AP far, w unused
    float4 cloud;      // x = lit cloud result live, y = atmosphere LUTs live
    ApParams ap;
};

// The froxel `fog.mode` selector — the analytic closed form vs. the volumetric froxel path.
static const uint FOG_MODE_ANALYTIC = 0u;
static const uint FOG_MODE_VOLUMETRIC = 1u;

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture2D<float4> target;
[[vk::binding(1, 0)]] ConstantBuffer<FogParams> params;
[[vk::binding(2, 0)]] Sampler2D depthTex;
[[vk::binding(3, 0)]] Sampler2D skyViewLut;
// The integrated froxel volume (rgb = in-scatter, a = transmittance), sampled trilinearly at
// `(screenUV, W)` in volumetric mode. Bound to the 1x1x1 fallback in analytic mode (never read there).
[[vk::binding(4, 0)]] Sampler3D<float4> froxelIntegration;
// The aerial-perspective volume (rgb = atmosphere in-scatter, a = mean transmittance), sampled at
// `(screenUV, W_ap)` when the atmosphere is live. Gated by `aerial.x`, so untouched when AP is off.
[[vk::binding(5, 0)]] Sampler3D<float4> aerialVolume;
[[vk::binding(6, 0)]] Sampler2D<float4> transmittanceLut;
[[vk::binding(7, 0)]] Sampler2D<float4> multiScatterLut;
[[vk::binding(8, 0)]] Sampler2D<float4> cloudBuffer;
[[vk::binding(9, 0)]] Sampler2D<float> cloudDepth;

// Closed-form line integral of exp2(-falloff*y) along the ray. The `abs(f) <= 1e-4` Taylor branch is
// the numerically critical detail for near-horizontal rays, where `(1 - exp2(-f))/f` is 0/0.
float layerOpticalDepth(FogLayer l, float camY, float recvY, float rayLen, float startDist)
{
    if (l.density <= 0.0)
    {
        return 0.0;
    }
    float originDensity = l.density * exp2(-l.heightFalloff * (camY - l.height));
    float f = l.heightFalloff * (recvY - camY);
    float lineInt = abs(f) > 1e-4 ? (1.0 - exp2(-f)) / f
                                  : (LN2 - 0.5 * LN2 * LN2 * f);
    return originDensity * lineInt * max(rayLen - startDist, 0.0);
}

// The Hillaire sky-view LUT tint for a world-space view direction: the inverse of the LUT's
// (azimuth, densified-elevation) parameterization built by `atmos_skyview.slang`.
float3 skyViewTint(float3 viewDir)
{
    float azimuth = atan2(viewDir.z, viewDir.x);
    if (azimuth < 0.0)
    {
        azimuth += 2.0 * PI;
    }
    float u = azimuth / (2.0 * PI);
    float elevation = asin(clamp(viewDir.y, -1.0, 1.0));
    float vCentered = (elevation >= 0.0 ? 1.0 : -1.0) * sqrt(abs(elevation) / PI);
    float v = clamp(0.5 + vCentered, 0.0, 1.0);
    return skyViewLut.SampleLevel(float2(u, v), 0.0).rgb;
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

    float depth = depthTex.SampleLevel(uv, 0.0).r;
    // Reconstruct the receiver's world position. The projection is Y-flipped, so NDC->UV is a plain
    // `*0.5 + 0.5` on both axes; the inverse is `uv * 2 - 1`.
    float4 clip = float4(uv * 2.0 - 1.0, depth, 1.0);
    float4 worldH = mul(params.invViewProj, clip);
    float3 worldPos = worldH.xyz / worldH.w;

    float3 toRecv = worldPos - params.cameraPos;
    float rayLen = length(toRecv);
    if (rayLen <= 1e-5)
    {
        return;
    }
    float3 viewDir = toRecv / rayLen;

    bool fogEnabled = params.fogEnabled > 0.5;
    bool volumetric = uint(params.froxel.x + 0.5) == FOG_MODE_VOLUMETRIC;

    // Aerial perspective (independent medium): sample the AP volume at this receiver's `(screenUV, W_ap)`
    // and carry `(inScatter_aerial, T_aerial)`. Identity (T=1, inScatter=0) when the atmosphere is off.
    float apT = 1.0;
    float3 apInScatter = float3(0.0, 0.0, 0.0);
    if (params.aerial.x > 0.5)
    {
        float apNear = params.aerial.y;
        float apFar = params.aerial.z;
        float apViewZ = max(dot(toRecv, params.camForward), apNear);
        float apW = saturate(log(apViewZ / apNear) / log(apFar / apNear));
        float4 ap = aerialVolume.SampleLevel(float3(uv, apW), 0.0);
        apInScatter = ap.rgb;
        apT = ap.a;
    }

    float4 c = target[px];

    // Fog term on the shared ledger: `T_fog` + `inScatter_fog` (the fog's own added radiance). Neutral
    // when fog is disabled (composite runs only for AP). The height density is injected as the froxel
    // base medium in volumetric mode, so the analytic closed form is NOT re-applied there (no double-count).
    float fogT = 1.0;
    float3 fogInScatter = float3(0.0, 0.0, 0.0);
    if (fogEnabled && volumetric)
    {
        float near = params.froxel.y;
        float far = params.froxel.z;
        float viewZ = max(dot(toRecv, params.camForward), near);
        float w = saturate(log(viewZ / near) / log(far / near));
        float4 vol = froxelIntegration.SampleLevel(float3(uv, w), 0.0);
        // The fog debug view mode (ViewMode::Fog): replace the frame with the froxel volume — the
        // in-scattered radiance (rgb) plus the fog opacity `1 - transmittance` as a grey floor, so
        // both the god-ray in-scatter and the density structure read directly (AP is not shown here).
        if (params.froxel.w > 0.5)
        {
            target[px] = float4(vol.rgb + (1.0 - vol.a), c.a);
            return;
        }
        fogT = vol.a;
        fogInScatter = vol.rgb;
    }
    else if (fogEnabled)
    {
        float tau = layerOpticalDepth(params.layer0, params.cameraPos.y, worldPos.y, rayLen, params.startDistance)
                  + layerOpticalDepth(params.layer1, params.cameraPos.y, worldPos.y, rayLen, params.startDistance);
        fogT = max(exp2(-tau), 1.0 - params.maxOpacity);

        // Sample the sky-view LUT only when the atmosphere baked it; otherwise the LUT holds
        // stale/undefined contents (its memory is valid, but a NaN there would poison the lerp). This
        // is the §5 horizon-hue coherence: near fog reddens toward the sun exactly as the sky does.
        float3 tint = params.useSkyLut > 0.5 ? skyViewTint(viewDir) : float3(1.0, 1.0, 1.0);
        float3 inscatter = params.albedo * tint + params.emissive;
        float sunAmount = saturate(dot(viewDir, params.sunDir) * 0.5 + 0.5);
        float3 sun = params.dirColor * pow(sunAmount, params.dirExponent);
        fogInScatter = (inscatter + sun) * (1.0 - fogT);
    }

    float cloudT = 1.0;
    float3 cloudL = float3(0.0);
    if (params.cloud.x > 0.5)
    {
        float4 cloudSample = cloudBuffer.SampleLevel(uv, 0.0);
        cloudT = saturate(cloudSample.a);
        if (cloudT < 0.99999)
        {
            float dCloud = cloudDepth.SampleLevel(uv, 0.0);
            cloudL = cloudSample.rgb;
            if (params.cloud.y > 0.5)
            {
                AtmoSample atmosphere = marchAtmosphere(
                    atmosphereObserverPosition(params.ap),
                    viewDir,
                    normalize(params.ap.sunDir.xyz),
                    max(dCloud, 0.0) * params.ap.apPlanes.w,
                    transmittanceLut,
                    multiScatterLut,
                    params.ap);
                cloudL = cloudSample.rgb * atmosphere.transmittance
                       + (1.0 - cloudT) * atmosphere.inScatter;
            }
        }
    }

    // One premultiplied front-to-back ledger. The background sees all three media; the cloud
    // radiance carries its own camera-to-cloud atmosphere and is attenuated only by nearer fog.
    float T = fogT * apT * cloudT;
    float3 L = c.rgb * T
             + fogInScatter
             + fogT * apInScatter
             + fogT * cloudL;
    target[px] = float4(L, c.a);
}
